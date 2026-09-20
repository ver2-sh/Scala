# Security

Scala is preparing its first public developer release; there is no published
binary release or long-term support branch yet. Security fixes target the current
source branch and, once published, the latest stable release.

Report suspected vulnerabilities privately to the existing maintainer contact,
Eugene at **dev@madebyeugene.com**. Include the affected revision/version, platform,
impact and a minimal reproduction. Do not include API keys, tokens, private model
content or user configuration. Please allow investigation before public disclosure;
no response deadline or bounty is promised. Use repository issues for non-sensitive
bugs and dependency maintenance reports.

Release downloads trust HTTPS and the publisher. Archive SHA-256 checks are not
publisher signatures. Native platform validation and signing status are documented
in [release maintenance](docs/releases.md). Keep the API bound to trusted interfaces,
protect Scala credentials, and treat model sources and separately installed native
runtimes as independent trust decisions.
