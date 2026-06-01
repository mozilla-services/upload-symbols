# Unreleased

This release is the first feature-complete version with support for version 1 of the Mozilla Symbols Server upload API.

## Preflight requests

The CLI now sends preflight requests before uploading any files to verify the authentication token and receive the configuration of the OTLP collector to send telemetry to.

* [bug-2037391: Send a preflight request to /upload/auth_info/.](https://github.com/mozilla-services/upload-symbols/pull/15)

## OpenTelemetry traces and metrics

If the server response contains an OTLP configuration, the CLI will send traces and metrics to the endpoint returned by the server.

* [bug-2037395: Send traces to an OTLP collector.](https://github.com/mozilla-services/upload-symbols/pull/17)
* [bug-2037395: Export some metrics to the OTLP collector.](https://github.com/mozilla-services/upload-symbols/pull/18)
