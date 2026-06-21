pub fn init() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("notm=info,warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
