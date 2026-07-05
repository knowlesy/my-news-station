"""Unit tests for the scraper's pure functions.

Run with:  python -m pytest scraper/tests/
(needs the scraper's requirements installed — see local-setup.sh)
"""
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))
import scraper  # noqa: E402


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
