# Gemini response contract and isolated scraper tests

Gemini responses now combine visible text parts and exclude thought parts. Invalid JSON, malformed response shapes, blocked requests or responses, missing candidates, and empty or nontext output raise a bounded diagnostic instead of returning unusable content or failing with an indexing error.

HTTP and transport errors report the configured model and relevant status or error category without logging provider bodies, prompts, or API keys. A model-not-found response asks for an availability check instead of recommending a replacement model. Backend and model defaults, request settings, the existing four-attempt transient retry limit, retry schedule, and provider-advertised delay handling are unchanged.

Fake-response regressions cover multipart output, thought filtering, rejected response shapes, error redaction, and retries. CI now runs the scraper tests on Python 3.12 with empty provider credentials and scratch data and cookie paths; the tests neutralize dotenv before imports and block network requests.

This change corrects the source response contract. It does not diagnose the cluster's actual provider or model failure, approve a provider or model change, or modify a live PVC or Job. Runtime diagnosis and isolated end-to-end acceptance remain separate work.
