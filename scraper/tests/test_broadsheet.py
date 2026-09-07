"""Unit tests for the broadsheet newspaper generator."""

import os
import sys
import unittest
from pathlib import Path

# Add scraper directory to sys.path
sys.path.insert(0, str(Path(__file__).parent.parent))

from broadsheet import (
    format_display_date,
    is_broadsheet_enabled,
    render_broadsheet_html,
    render_spreads_html,
)


class TestBroadsheet(unittest.TestCase):
    def setUp(self):
        self.orig_env = os.environ.get("ENABLE_BROADSHEET_EDITION")
        if "ENABLE_BROADSHEET_EDITION" in os.environ:
            del os.environ["ENABLE_BROADSHEET_EDITION"]

    def tearDown(self):
        if self.orig_env is not None:
            os.environ["ENABLE_BROADSHEET_EDITION"] = self.orig_env
        elif "ENABLE_BROADSHEET_EDITION" in os.environ:
            del os.environ["ENABLE_BROADSHEET_EDITION"]

    def test_toggle_default_is_disabled(self):
        self.assertFalse(is_broadsheet_enabled({}))
        self.assertFalse(is_broadsheet_enabled(None))

    def test_toggle_config_keys(self):
        # Top-level enable_broadsheet_edition
        self.assertTrue(is_broadsheet_enabled({"enable_broadsheet_edition": True}))
        self.assertFalse(is_broadsheet_enabled({"enable_broadsheet_edition": False}))

        # Nested editions.broadsheet.enabled
        self.assertTrue(is_broadsheet_enabled({"editions": {"broadsheet": {"enabled": True}}}))
        self.assertFalse(is_broadsheet_enabled({"editions": {"broadsheet": {"enabled": False}}}))

    def test_toggle_env_var_precedence(self):
        # Env var True overrides config False
        os.environ["ENABLE_BROADSHEET_EDITION"] = "true"
        self.assertTrue(is_broadsheet_enabled({"enable_broadsheet_edition": False}))
        os.environ["ENABLE_BROADSHEET_EDITION"] = "1"
        self.assertTrue(is_broadsheet_enabled({}))

        # Env var False overrides config True
        os.environ["ENABLE_BROADSHEET_EDITION"] = "false"
        self.assertFalse(is_broadsheet_enabled({"enable_broadsheet_edition": True}))
        os.environ["ENABLE_BROADSHEET_EDITION"] = "0"
        self.assertFalse(is_broadsheet_enabled({"editions": {"broadsheet": {"enabled": True}}}))

    def test_format_display_date(self):
        self.assertEqual(format_display_date("20260907"), "Monday, September 7, 2026")
        self.assertEqual(format_display_date("20260907-143000"), "Monday, September 7, 2026")

    def test_render_spreads_html_structure(self):
        articles = [
            {
                "title": "Major Breakthrough in Nuclear Fusion",
                "source": "Science Daily",
                "url": "https://example.com/fusion",
                "author": "Dr. Eleanor Vance",
                "text": "Scientists at the National Ignition Facility announced net energy gain.\nCommercial viability is closer.",
            },
            {
                "title": "Global Markets Rally Following Policy Shift",
                "source": "Financial Times",
                "url": "https://example.com/markets",
                "author": "Marcus Sterling",
                "text": "Equity indexes surged worldwide following central bank remarks.\nTech stocks led the charge.",
            },
            {
                "title": "New High-Speed Rail Network Opens",
                "source": "Transport Review",
                "url": "https://example.com/rail",
                "author": "Clara Oswald",
                "text": "The inaugural line connects the capital to the northern regions.",
            },
        ]

        html = render_spreads_html(articles, "20260907")

        # Verify dual-page spread elements
        self.assertIn("spread-container", html)
        self.assertIn("masthead-wrap", html)
        self.assertIn("THE DAILY BROADSHEET", html)
        self.assertIn("left-half", html)
        self.assertIn("spine-gutter", html)
        self.assertIn("spine-rule", html)
        self.assertIn("right-half", html)

        # Verify lead story drop cap styling and articles
        self.assertIn("lead-story", html)
        self.assertIn("banner", html)
        self.assertIn("Major Breakthrough in Nuclear Fusion", html)
        self.assertIn("Global Markets Rally Following Policy Shift", html)

    def test_render_broadsheet_html_complete_document(self):
        articles = [
            {
                "title": "Quantum Computing Milestone Reached",
                "source": "Tech News",
                "url": "https://example.com/quantum",
                "author": "Alice Reed",
                "text": "Quantum coherence maintained for over ten minutes in silicon.",
            }
        ]

        full_html = render_broadsheet_html(articles, "20260907")
        self.assertTrue(full_html.startswith("<!DOCTYPE html>"))
        self.assertIn("<title>THE DAILY BROADSHEET — Monday, September 7, 2026</title>", full_html)
        self.assertIn("420mm 297mm landscape", full_html)
        self.assertIn("column-count: 3", full_html)
        self.assertIn("Quantum Computing Milestone Reached", full_html)


if __name__ == "__main__":
    unittest.main()
