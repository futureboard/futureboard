fn main() {
    sphere_webview::runtime::log_process_entry();
    let mut application = match sphere_webview::scheme::plugin_scheme_app() {
        Ok(application) => application,
        Err(error) => {
            eprintln!(
                "[cef-helper] role=setup-failure pid={} error={error}",
                std::process::id()
            );
            std::process::exit(1);
        }
    };
    match sphere_webview::runtime::execute_subprocess(Some(&mut application)) {
        Ok(sphere_webview::runtime::ProcessDispatch::SubprocessExit(code)) => {
            eprintln!(
                "[cef-helper] role=subprocess pid={} exit_code={code}",
                std::process::id()
            );
            std::process::exit(code);
        }
        Ok(sphere_webview::runtime::ProcessDispatch::BrowserProcess) => {
            eprintln!(
                "[cef-helper] role=invalid-browser pid={} refusing_main_startup=true",
                std::process::id()
            );
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!(
                "[cef-helper] role=execution-failure pid={} error={error}",
                std::process::id()
            );
            std::process::exit(1);
        }
    }
}
