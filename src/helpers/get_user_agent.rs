pub fn get_user_agent() -> String {
    format!("lumin/{}", env!("CARGO_PKG_VERSION"),)
}
