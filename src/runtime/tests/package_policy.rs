use super::*;
use https_fixture::{HttpsFixture, WHEEL};

#[tokio::test]
async fn download_policy_checks_redirects_and_scopes_cached_responses() {
    let fixture = HttpsFixture::start().await;
    let config: AppConfig = toml::from_str(
        "allowed_hosts = ['  General.TEST ', ' shared.test  ']
         runtime_package_installation_allowed_hosts = [' PACKAGE.Test ', 'Shared.TEST']
         runtime_package_installation = false",
    )
    .unwrap();
    let config = RuntimeConfig::new(&config, || fixture.client_builder()).unwrap();

    let allowed = fixture.url("general.test", "/allowed-redirect");
    let download = fetch_url(&config, true, &allowed).await.unwrap();
    assert_eq!(download.bytes, b"fixture data");
    assert_eq!(download.content_type, "text/plain");
    assert!(Arc::ptr_eq(
        &download,
        &fetch_url(&config, true, &allowed).await.unwrap()
    ));

    for (url, message) in [
        (
            fixture.url("general.test", "/blocked-host"),
            "network host is not allowlisted: unlisted.test",
        ),
        (
            fixture.url("general.test", "/blocked-scheme"),
            "only HTTPS URLs are allowed",
        ),
        (
            fixture.url("notgeneral.test", "/download"),
            "network host is not allowlisted: notgeneral.test",
        ),
        (
            fixture.url("general.test.unlisted.test", "/download"),
            "network host is not allowlisted: general.test.unlisted.test",
        ),
        (
            fixture.url("pypi.org", "/download"),
            "network host is not allowlisted: pypi.org",
        ),
    ] {
        let error = fetch_url(&config, true, &url).await.unwrap_err();
        assert!(format!("{error:#}").contains(message), "{error:#}");
    }

    let shared = fixture.url("shared.test", "/initialization-redirect");
    assert_eq!(
        fetch_url(&config, false, &shared).await.unwrap().bytes,
        b"fixture data"
    );
    let error = fetch_url(&config, true, &shared).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("network host is not allowlisted: package.test"),
        "{error:#}"
    );
    let shared = fixture.url("shared.test", "/download");
    let startup = fetch_url(&config, false, &shared).await.unwrap();
    let user = fetch_url(&config, true, &shared).await.unwrap();
    assert!(!Arc::ptr_eq(&startup, &user));
    assert_eq!(user.content_type, "text/plain");

    let general = fixture.url("general.test", "/download");
    assert!(
        fetch_url(&config, false, &general)
            .await
            .unwrap_err()
            .to_string()
            .contains("network host is not allowlisted: general.test")
    );
}

#[tokio::test]
async fn download_policy_uses_default_and_explicit_package_hosts() {
    let fixture = HttpsFixture::start().await;
    let pypi = fixture.url("pypi.org", "/download");
    let unlisted = fixture.url("unlisted.test", "/download");
    for (source, pypi_allowed) in [
        ("", true),
        (
            "runtime_package_installation_allowed_hosts = ['package.test']",
            false,
        ),
        ("runtime_package_installation_allowed_hosts = []", false),
    ] {
        let config: AppConfig = toml::from_str(source).unwrap();
        let config = RuntimeConfig::new(&config, || fixture.client_builder()).unwrap();
        for user_execution_started in [false, true] {
            assert_eq!(
                fetch_url(&config, user_execution_started, &pypi)
                    .await
                    .is_ok(),
                pypi_allowed
            );
            let error = fetch_url(&config, user_execution_started, &unlisted)
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("network host is not allowlisted: unlisted.test")
            );
        }
    }
}

#[tokio::test]
async fn local_pyodide_assets_and_cdn_fallback_use_the_active_policy() {
    let asset_dir =
        std::env::temp_dir().join(format!("ermcp-download-assets-{}", std::process::id()));
    fs::create_dir(&asset_dir).unwrap();
    fs::write(asset_dir.join("test.json"), b"{}").unwrap();
    let mut config: AppConfig = toml::from_str(
        "allowed_hosts = ['cdn.jsdelivr.net']
         runtime_package_installation_allowed_hosts = []
         runtime_package_installation = false",
    )
    .unwrap();
    config.asset_dir = Some(asset_dir.clone());
    let config = RuntimeConfig::new(&config, reqwest::Client::builder).unwrap();
    let url = "https://pyodide.local/test.json";
    let startup = fetch_url(&config, false, url).await;
    let user = fetch_url(&config, true, url).await;
    let fallback = fetch_url(&config, false, "https://pyodide.local/missing.whl").await;
    fs::remove_dir_all(asset_dir).unwrap();

    let download = startup.unwrap();
    assert_eq!(download.bytes, b"{}");
    assert_eq!(download.content_type, "application/json");
    assert!(
        user.unwrap_err()
            .to_string()
            .contains("network host is not allowlisted: pyodide.local")
    );
    let error = fallback.unwrap_err();
    assert!(
        format!("{error:#}").contains("network host is not allowlisted: cdn.jsdelivr.net"),
        "{error:#}"
    );
}

async fn call_download_tool(
    config: &AppConfig,
    runtime: PyodideRuntime,
    url: &str,
) -> RunPythonCodeOutput {
    let server = crate::CodeInterpreterMcpServer {
        runtime,
        input_limits: config.input_limits.validate().unwrap(),
        code_snippets: config.code_snippets.clone(),
        search_min_hits: config.search.min_hits,
        tool_router: crate::CodeInterpreterMcpServer::configured_tool_router(config),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let router = crate::http_router(config, server);
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let post = || {
        client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
    };
    let response = post().body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"download-test","version":"1"}}}"#)
        .send().await.unwrap().error_for_status().unwrap();
    let session = response.headers()["mcp-session-id"].clone();
    response.bytes().await.unwrap();
    let post = || {
        post()
            .header("Mcp-Session-Id", session.clone())
            .header("MCP-Protocol-Version", "2025-11-25")
    };
    post()
        .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let code = format!(
        "from pathlib import Path\nfrom pyodide.http import pyfetch\nresponse = await pyfetch({url:?})\nassert response.headers['content-type'] == 'text/plain'\nPath('download.txt').write_bytes(await response.bytes())"
    );
    let body = post()
        .body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "run_python_code", "arguments": { "code": code } },
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    task.abort();
    let payload = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .find(|data| !data.is_empty())
        .unwrap_or(&body);
    let message: serde_json::Value = serde_json::from_str(payload).unwrap();
    serde_json::from_value(message["result"]["structuredContent"].clone())
        .unwrap_or_else(|error| panic!("{error}: {message}"))
}

#[tokio::test]
#[ignore = "requires npm ci and the pinned micropip wheel"]
async fn package_policy_in_fresh_sandboxes() {
    let fixture = HttpsFixture::start().await;
    let wheel = fixture.url("package.test", &format!("/{WHEEL}"));
    let cache_dir =
        std::env::temp_dir().join(format!("ermcp-package-layer-{}", std::process::id()));
    let mut config: AppConfig = toml::from_str(
        "allowed_hosts = ['general.test', 'shared.test']
         runtime_package_installation_allowed_hosts = ['package.test', 'shared.test']
         runtime_package_installation = false",
    )
    .unwrap();
    config.preinstall_packages = vec![wheel.clone()];
    config.package_cache_dir = Some(cache_dir.clone());
    let runtime = PyodideRuntime::start(
        &config,
        RuntimeConfig::new(&config, || fixture.client_builder()).unwrap(),
    )
    .await
    .unwrap();

    for _ in 0..2 {
        let output = run(
            &runtime,
            "import fixture_package, json\nprint(fixture_package.VALUE)",
        )
        .await;
        assert!(output.success, "{output:?}");
        assert_eq!(output.stdout, "42\n");
    }
    let output = call_download_tool(
        &config,
        runtime.clone(),
        &fixture.url("general.test", "/allowed-redirect"),
    )
    .await;
    assert!(output.success, "{output:?}");
    assert_eq!(output.files.len(), 1);
    assert_eq!(output.files[0].path, "download.txt");
    assert_eq!(output.files[0].content_base64, "Zml4dHVyZSBkYXRh");

    let output = run(&runtime, "import scipy").await;
    assert!(!output.success, "{output:?}");
    assert!(
        output
            .error
            .unwrap()
            .traceback
            .unwrap()
            .contains("No module named 'scipy'")
    );
    for code in [
        "import micropip\nawait micropip.install('fixture-package')",
        "import importlib, micropip\nimportlib.reload(micropip)\nawait micropip.install('fixture-package')",
        "import sys, micropip\ndel sys.modules['micropip']\nimport micropip\nawait micropip.install('fixture-package')",
        "import pyodide_js\nawait pyodide_js.loadPackage('scipy')",
        "import pyodide_js\nawait pyodide_js.loadPackagesFromImports('import scipy')",
    ] {
        let output = run(&runtime, code).await;
        assert!(!output.success, "{code}: {output:?}");
        assert!(!output.timed_out, "{output:?}");
        assert!(
            output
                .error
                .unwrap()
                .message
                .contains("Runtime package installation is disabled")
        );
    }
    for host in ["package.test", "unlisted.test", "pypi.org"] {
        let url = fixture.url(host, "/download");
        let code = format!("from pyodide.http import pyfetch\nawait pyfetch({url:?})");
        let output = run(&runtime, &code).await;
        assert!(!output.success, "{code}: {output:?}");
        let message = output.error.unwrap().message;
        assert!(
            message.contains(&format!("network host is not allowlisted: {host}")),
            "{message}"
        );
    }
    let shared = fixture.url("shared.test", "/download");
    let output = run(
        &runtime,
        &format!(
            "from pyodide.http import pyfetch\nprint(await (await pyfetch({shared:?})).string())"
        ),
    )
    .await;
    assert!(output.success, "{output:?}");
    assert_eq!(output.stdout, "fixture data\n");
    runtime.shutdown();
    assert!(cache_dir.join("package-layer.json").is_file());
    fs::remove_dir_all(cache_dir).unwrap();

    config.runtime_package_installation = true;
    config.preinstall_packages.clear();
    let runtime = PyodideRuntime::start(
        &config,
        RuntimeConfig::new(&config, || fixture.client_builder()).unwrap(),
    )
    .await
    .unwrap();
    let code = format!(
        "import micropip\nawait micropip.install({wheel:?})\nimport fixture_package\nprint(fixture_package.VALUE)"
    );
    let output = run(&runtime, &code).await;
    assert!(output.success, "{output:?}");
    assert_eq!(output.stdout, "42\n");
    let general = fixture.url("general.test", "/download");
    let output = run(
        &runtime,
        &format!(
            "from pyodide.http import pyfetch\nprint(await (await pyfetch({general:?})).string())"
        ),
    )
    .await;
    assert!(output.success, "{output:?}");
    assert_eq!(output.stdout, "fixture data\n");
    runtime.shutdown();

    config.preinstall_packages = vec![fixture.url("general.test", &format!("/{WHEEL}"))];
    let error = PyodideRuntime::start(
        &config,
        RuntimeConfig::new(&config, || fixture.client_builder()).unwrap(),
    )
    .await
    .err()
    .expect("startup must reject a package on a general-only host");
    assert!(
        format!("{error:#}").contains("network host is not allowlisted: general.test"),
        "{error:#}"
    );
}

#[tokio::test]
#[ignore = "requires npm ci and HTTPS package downloads"]
async fn default_policy_installs_pyodide_and_pypi_packages() {
    let config: AppConfig = toml::from_str("").unwrap();
    let runtime = PyodideRuntime::new(&config).await.unwrap();
    let output = run(&runtime, "import numpy\nimport micropip\nawait micropip.install('snowballstemmer==3.0.1')\nimport snowballstemmer\nprint(numpy.arange(3).sum())").await;
    assert!(output.success, "{output:?}");
    assert_eq!(output.stdout, "3\n");

    let output = run(
        &runtime,
        "from pyodide.http import pyfetch\nawait pyfetch('https://unlisted.test/download')",
    )
    .await;
    assert!(!output.success, "{output:?}");
    assert!(
        output
            .error
            .unwrap()
            .message
            .contains("network host is not allowlisted: unlisted.test")
    );
    runtime.shutdown();

    let mut config: AppConfig = toml::from_str(
        "preinstall_packages = ['numpy', 'snowballstemmer==3.0.1']
         runtime_package_installation = false",
    )
    .unwrap();
    let cache_dir = std::env::temp_dir().join(format!(
        "ermcp-default-package-layer-{}",
        std::process::id()
    ));
    config.package_cache_dir = Some(cache_dir.clone());
    let runtime = PyodideRuntime::new(&config).await.unwrap();
    let output = run(
        &runtime,
        "import numpy, snowballstemmer\nprint(numpy.arange(3).sum())",
    )
    .await;
    runtime.shutdown();
    fs::remove_dir_all(cache_dir).unwrap();
    assert!(output.success, "{output:?}");
    assert_eq!(output.stdout, "3\n");
}
