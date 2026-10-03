"""Unit tests for the scraper's pure functions.

Run with:  python -m pytest scraper/tests/
(needs the scraper's requirements installed — see local-setup.sh)
"""
import json
import sys
import types
from pathlib import Path

dotenv_stub = types.ModuleType("dotenv")
dotenv_stub.load_dotenv = lambda *args, **kwargs: False
dotenv_stub.dotenv_values = lambda *args, **kwargs: {}
sys.modules["dotenv"] = dotenv_stub

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))
import scraper  # noqa: E402


@pytest.fixture(autouse=True)
def forbid_network(monkeypatch):
    def blocked_request(*args, **kwargs):
        raise AssertionError("Network requests are forbidden in scraper unit tests")
    monkeypatch.setattr(scraper.requests.sessions.Session, "request", blocked_request)


class FakeGeminiResponse:
    def __init__(self, status=200, body=None, text="FAKE_PROVIDER_BODY_MARKER"):
        self.status_code = status
        self.body = body
        self.text = text

    def json(self):
        if isinstance(self.body, Exception):
            raise self.body
        return self.body

    def raise_for_status(self):
        if self.status_code >= 400:
            raise scraper.requests.HTTPError(self.text)


@pytest.fixture
def gemini_api(monkeypatch):
    monkeypatch.setattr(scraper, "GOOGLE_AI_KEY", "FAKE_KEY_MARKER")
    monkeypatch.setattr(scraper, "GEMINI_MODEL", "fixture-model")
    state = types.SimpleNamespace(responses=[], calls=[], sleeps=[])

    def fake_post(url, **kwargs):
        state.calls.append((url, kwargs))
        if not state.responses:
            raise AssertionError("Unexpected fake Gemini request")
        response = state.responses.pop(0)
        if isinstance(response, Exception):
            raise response
        return response

    monkeypatch.setattr(scraper.requests, "post", fake_post)
    monkeypatch.setattr(scraper.time, "sleep", state.sleeps.append)
    return state


def test_gemini_joins_visible_text_parts_and_preserves_request_contract(gemini_api):
    gemini_api.responses.append(FakeGeminiResponse(body={"candidates": [{"content": {"parts": [
        {"text": "FAKE_THOUGHT_MARKER", "thought": True},
        {"inlineData": {"data": "nontext-fixture"}},
        {"text": "First "}, {"text": "second", "thought": False},
    ]}}]}))
    assert scraper.call_gemini("FAKE_PROMPT_MARKER") == "First second"
    url, arguments = gemini_api.calls[0]
    assert url.endswith("/models/fixture-model:generateContent")
    assert "FAKE_KEY_MARKER" not in url
    assert arguments["headers"] == {"x-goog-api-key": "FAKE_KEY_MARKER"}
    assert arguments["json"]["contents"] == [{"parts": [{"text": "FAKE_PROMPT_MARKER"}]}]
    assert arguments["json"]["generationConfig"] == {"maxOutputTokens": 16384, "temperature": 0.4}
    assert arguments["timeout"] == 180


@pytest.mark.parametrize("body", [
    None, [], {}, {"candidates": []}, {"candidates": {}}, {"candidates": [None]},
    {"candidates": [{"content": None}]}, {"candidates": [{"content": {"parts": {}}}]},
    {"candidates": [{"content": {"parts": []}}]},
    {"candidates": [{"content": {"parts": [{"text": "   "}]}}]},
    {"candidates": [{"content": {"parts": [{"text": 123}]}}]},
    {"candidates": [{"content": {"parts": [None, "malformed", {"inlineData": {"data": "FAKE_PROVIDER_BODY_MARKER"}}]}}]},
    {"candidates": [{"content": {"parts": [{"text": "FAKE_PROVIDER_BODY_MARKER", "thought": True}]}}]},
    {"promptFeedback": {"blockReason": "SAFETY", "detail": "FAKE_PROVIDER_BODY_MARKER"}},
    {"candidates": [{"finishReason": "SAFETY", "content": {"parts": [{"text": "FAKE_PROVIDER_BODY_MARKER"}]}}]},
    json.JSONDecodeError("FAKE_PROVIDER_BODY_MARKER", "FAKE_KEY_MARKER FAKE_PROMPT_MARKER", 0),
])
def test_gemini_rejects_unusable_output_without_provider_content(gemini_api, caplog, body):
    gemini_api.responses.append(FakeGeminiResponse(body=body))
    with pytest.raises(RuntimeError, match="Gemini.*fixture-model") as error:
        scraper.call_gemini("FAKE_PROMPT_MARKER")
    diagnostic = str(error.value) + caplog.text
    for marker in ["FAKE_PROVIDER_BODY_MARKER", "FAKE_KEY_MARKER", "FAKE_PROMPT_MARKER"]:
        assert marker not in diagnostic
    assert len(str(error.value)) < 200
    assert len(gemini_api.calls) == 1
    assert gemini_api.sleeps == []


@pytest.mark.parametrize("status", [400, 401, 403, 404])
def test_gemini_http_errors_redact_body_and_do_not_guess_a_model(gemini_api, caplog, status):
    gemini_api.responses.append(FakeGeminiResponse(status=status, text="FAKE_PROVIDER_BODY_MARKER FAKE_KEY_MARKER FAKE_PROMPT_MARKER"))
    with pytest.raises(RuntimeError) as error:
        scraper.call_gemini("FAKE_PROMPT_MARKER")
    diagnostic = str(error.value) + caplog.text
    assert str(status) in diagnostic
    assert "fixture-model" in diagnostic
    assert "gemini-2.5-flash" not in diagnostic
    for marker in ["FAKE_PROVIDER_BODY_MARKER", "FAKE_KEY_MARKER", "FAKE_PROMPT_MARKER"]:
        assert marker not in diagnostic
    if status == 404:
        assert "availability" in str(error.value)
    assert gemini_api.sleeps == []


def test_gemini_preserves_transient_retry_sequence_and_provider_delay(gemini_api, caplog):
    gemini_api.responses.extend([
        FakeGeminiResponse(status=429, text="FAKE_PROVIDER_BODY_MARKER retry in 92.6s"),
        FakeGeminiResponse(status=500), FakeGeminiResponse(status=503),
        FakeGeminiResponse(body={"candidates": [{"content": {"parts": [{"text": "success"}]}}]}),
    ])
    assert scraper.call_gemini("FAKE_PROMPT_MARKER") == "success"
    assert gemini_api.sleeps == [95, 60, 120]
    assert len(gemini_api.calls) == 4
    assert "FAKE_PROVIDER_BODY_MARKER" not in caplog.text


@pytest.mark.parametrize("status", [429, 500, 503])
def test_gemini_stops_after_four_transient_attempts(gemini_api, caplog, status):
    gemini_api.responses.extend(FakeGeminiResponse(status=status) for _ in range(4))
    with pytest.raises(RuntimeError, match=f"HTTP {status}"):
        scraper.call_gemini("FAKE_PROMPT_MARKER")
    assert gemini_api.sleeps == [30, 60, 120]
    assert len(gemini_api.calls) == 4
    assert "FAKE_PROVIDER_BODY_MARKER" not in caplog.text


@pytest.mark.parametrize("exception_type", [scraper.requests.Timeout, scraper.requests.ConnectionError])
def test_gemini_transport_errors_are_sanitized_without_new_retries(gemini_api, caplog, exception_type):
    gemini_api.responses.append(exception_type("FAKE_PROVIDER_BODY_MARKER FAKE_KEY_MARKER FAKE_PROMPT_MARKER"))
    with pytest.raises(RuntimeError, match="transport error.*fixture-model") as error:
        scraper.call_gemini("FAKE_PROMPT_MARKER")
    diagnostic = str(error.value) + caplog.text
    for marker in ["FAKE_PROVIDER_BODY_MARKER", "FAKE_KEY_MARKER", "FAKE_PROMPT_MARKER"]:
        assert marker not in diagnostic
    assert error.value.__suppress_context__ is True
    assert gemini_api.sleeps == []
    assert len(gemini_api.calls) == 1


# ── extract_xml_block ────────────────────────────────────────────────

def test_extract_xml_block_basic():
    text = "junk <short_radio>hello world</short_radio> junk"
    assert scraper.extract_xml_block(text, "short_radio") == "hello world"


def test_extract_xml_block_multiline_and_case():
    text = "<TLDR_DIGEST>\nline one\nline two\n</TLDR_DIGEST>"
    assert scraper.extract_xml_block(text, "tldr_digest") == "line one\nline two"


def test_extract_xml_block_missing_returns_empty():
    assert scraper.extract_xml_block("no tags here", "short_radio") == ""


# ── _parse_tldr_sections ─────────────────────────────────────────────

def test_parse_tldr_structured():
    text = (
        "[[BBC News]]\n"
        "- Story A :: What happened in A.\n"
        "- Story B :: What happened in B.\n"
        "[[GitHub Blog]]\n"
        "- Story C :: What happened in C.\n"
    )
    sections = scraper._parse_tldr_sections(text)
    assert [s for s, _ in sections] == ["BBC News", "GitHub Blog"]
    assert sections[0][1] == [
        ("Story A", "What happened in A."),
        ("Story B", "What happened in B."),
    ]
    assert sections[1][1] == [("Story C", "What happened in C.")]


def test_parse_tldr_tolerates_bullets_without_separator():
    sections = scraper._parse_tldr_sections("[[Src]]\n- just a sentence, no separator")
    assert sections == [("Src", [("", "just a sentence, no separator")])]


def test_parse_tldr_freeform_returns_empty():
    # No [[Source]] markers and no bullets → nothing parsed → caller
    # falls back to the single-chapter raw-text EPUB
    assert scraper._parse_tldr_sections("The LLM ignored the format entirely.") == []


# ── merge_todays_articles (same-day re-run = diff vs yesterday) ─────

def test_merge_restores_prior_articles_dedup_by_url(tmp_path, monkeypatch):
    monkeypatch.setattr(scraper, "DATA_DIR", tmp_path)
    prior = [
        {"title": "Morning story", "url": "http://x/a", "source": "BBC News"},
        {"title": "Dup of fresh", "url": "http://x/c", "source": "BBC News"},
    ]
    (tmp_path / "articles-20260704.json").write_text(json.dumps(prior))

    fresh = [{"title": "Afternoon story", "url": "http://x/c", "source": "BBC News"}]
    merged = scraper.merge_todays_articles(fresh, "20260704-140000")

    urls = [a["url"] for a in merged]
    assert urls == ["http://x/a", "http://x/c"]  # prior first, dup dropped


def test_merge_without_sidecar_is_noop(tmp_path, monkeypatch):
    monkeypatch.setattr(scraper, "DATA_DIR", tmp_path)
    fresh = [{"title": "t", "url": "http://x/a"}]
    assert scraper.merge_todays_articles(fresh, "20260704-060000") == fresh


# ── resolve_configured_sources (env > config > default) ─────────────

def test_sources_env_override_wins(monkeypatch):
    monkeypatch.setenv("SHORT_SOURCES", "Feed One,Feed Two")
    monkeypatch.delenv("LONG_SOURCES", raising=False)
    monkeypatch.setattr(scraper, "SOURCES_SHORT", ["Config Feed"])
    monkeypatch.setattr(scraper, "SOURCES_LONG", [])
    short, long_ = scraper.resolve_configured_sources()
    assert short == ["Feed One", "Feed Two"]
    assert long_ == ["BBC News", "Medium/tags/terraform"]  # hardcoded default


def test_sources_config_beats_default(monkeypatch):
    monkeypatch.delenv("SHORT_SOURCES", raising=False)
    monkeypatch.delenv("LONG_SOURCES", raising=False)
    monkeypatch.setattr(scraper, "SOURCES_SHORT", ["Config Feed"])
    monkeypatch.setattr(scraper, "SOURCES_LONG", ["Other Feed"])
    assert scraper.resolve_configured_sources() == (["Config Feed"], ["Other Feed"])


# ── is_paywalled_content ─────────────────────────────────────────────

@pytest.mark.parametrize("snippet", [
    "Sign up to read the full story",
    "This is a member-only story on Medium.",
    "SUBSCRIBE TO CONTINUE reading today",
])
def test_paywall_detected(snippet):
    assert scraper.is_paywalled_content(f"Intro paragraph. {snippet}. More.") is True


def test_paywall_clean_article_passes():
    assert scraper.is_paywalled_content("A normal article about Kubernetes.") is False


def test_paywall_empty_is_not_paywalled():
    assert scraper.is_paywalled_content("") is False


# ── build_prompt output contract (per-task token limiting) ──────────

ARTICLES = [
    {"title": "BBC tax story", "source": "BBC News", "url": "u1",
     "content": "BBC CONTENT", "audio_highlight": True},
    {"title": "Terraform guide", "source": "Medium/tags/terraform", "url": "u2",
     "content": "MEDIUM CONTENT", "audio_highlight": True},
]


@pytest.fixture(autouse=True)
def _default_sources(monkeypatch):
    monkeypatch.delenv("SHORT_SOURCES", raising=False)
    monkeypatch.delenv("LONG_SOURCES", raising=False)
    monkeypatch.setattr(scraper, "SOURCES_SHORT", ["BBC News"])
    monkeypatch.setattr(scraper, "SOURCES_LONG", ["Medium/tags/terraform"])


def test_prompt_full_requests_all_blocks():
    p = scraper.build_prompt(ARTICLES)
    for expected in ("<short_radio>", "<long_podcast>", "<tldr_digest>",
                     "BBC CONTENT", "MEDIUM CONTENT"):
        assert expected in p


def test_prompt_radio_only_excludes_podcast_material():
    p = scraper.build_prompt(ARTICLES, tracks=("radio",), include_tldr=False)
    assert "ONLY the following output block(s): <short_radio>" in p
    assert "BBC CONTENT" in p
    assert "MEDIUM CONTENT" not in p       # podcast-source article not paid for
    assert "TLDR INDEX" not in p


def test_prompt_tldr_only_has_no_audio_sections():
    p = scraper.build_prompt(ARTICLES, tracks=(), include_tldr=True)
    assert "ONLY the following output block(s): <tldr_digest>" in p
    assert "CRITICAL CONTENT FILTER" not in p
    assert "FULL DAILY POOL" not in p
    assert "TLDR INDEX" in p
