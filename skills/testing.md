---
name: testing
description: Writing and fixing tests that actually catch bugs
triggers: test, tests, testing, coverage, regression test, unit test, spec, assertion, mock
---
# Testing
Follow the project's existing runner and layout; never introduce a second framework. A regression test must fail before the fix and pass after: run it both ways. Test behavior and edge cases (empty, boundary, error, concurrent), not implementation details. Do not weaken, skip or delete a test to get green; change an expectation only when the intended behavior genuinely changed, and say so. Prefer real objects over mocks where cheap.
