fn main() {
    if std::env::args()
        .nth(1)
        .is_some_and(|value| value.starts_with("chrome-extension://"))
    {
        kitsupin_lib::native_host::run();
        return;
    }
    kitsupin_lib::run();
}
