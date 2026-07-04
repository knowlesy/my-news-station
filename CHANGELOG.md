# Changelog

All notable changes to my-news-station are documented here.
Format: [Date] - Brief description of changes

## [Unreleased]

## [2026-07-04]
- Fix: Rust 1.86 requirement for ICU crates (indexmap v2.14.0)
- Add: `.dockerignore` to reduce Docker build context (528KB → ~50KB)
- Update: Model versions to latest
  - `claude-opus-4-5` → `claude-opus-4-8`
  - `gemini-1.5-pro` → `gemini-2.0-flash`
- Chore: Add Kustomization for ArgoCD my-news-station deployment
- Chore: Delete dead build-cronjob.yaml (privileged, unused)

## [2026-07-03]
- Initial public release
- Multi-stage Docker build: Rust server + Python scraper
- Kubernetes deployment via ArgoCD (GitOps)
- Supports multiple LLM backends: claude_cli, claude_api, gemini
- Daily scraper CronJob with Playwright/Chromium
