# Changelog

## 0.13.3 — 2026-09-23

- Fix an AbortSignal listener leak: a request that settled before its signal fired left its abort listener attached, so a long-lived signal reused across calls accumulated one closure per request. The listener is now removed when the request settles.
- Isolate a throwing `onRouteGone` callback: it is now reported via `console.warn` and absorbed instead of escaping the read loop, where it was treated as an unexpected drop and tore down every route on the provider's connection.

## 0.11.1 — 2026-09-05

- Add per-route fault isolation during reconnect reopen, so one refused route no longer fails waiting calls on other routes.

## 0.11.0 — 2026-09-04

- Add opaque binary request bodies and wire-flag-driven binary replies.
- Add `callBinary()` for managed routes while keeping `call()` JSON-only.

## 0.8.2 — 2026-08-24

- Add capability-addressed provider resolution from the catalog capabilities mirror.
- Add deterministic plural resolution, singular ambiguity/unprovided errors, and local identifier validation.
