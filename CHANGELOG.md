# Release notes

## Unreleased

- Python can download files from HTTPS hosts in `allowed_hosts` while runtime
  package installation is disabled. The general download allowlist now
  defaults to empty. Every redirect must also pass the active allowlist.
- Roll out the updated configuration and image together. Move package mirrors
  from `allowed_hosts` to `runtime_package_installation_allowed_hosts`, or from
  `CODE_INTERPRETER_MCP_ALLOWED_HOSTS` to
  `CODE_INTERPRETER_MCP_RUNTIME_PACKAGE_INSTALLATION_ALLOWED_HOSTS` when using
  environment variables. Entries left in the general allowlist permit downloads
  during user execution even with runtime installation disabled. Omitting the
  package-host setting preserves the default hosts for Pyodide and PyPI.
