pub struct EnvVarRestoreGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarRestoreGuard {
    pub fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }

    pub fn remove(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        std::env::remove_var(name);
        Self { name, previous }
    }
}

impl Drop for EnvVarRestoreGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EnvVarRestoreGuard;

    const RESTORE_GUARD_TEST_ENV: &str = "FLAPJACK_TEST_ENV_VAR_RESTORE_GUARD";

    #[test]
    #[serial_test::serial(env_var_restore_guard)]
    fn restores_absent_and_present_values() {
        let _original_value = EnvVarRestoreGuard::remove(RESTORE_GUARD_TEST_ENV);

        {
            let _set_value = EnvVarRestoreGuard::set(RESTORE_GUARD_TEST_ENV, "temporary");
            assert_eq!(
                std::env::var_os(RESTORE_GUARD_TEST_ENV).as_deref(),
                Some(std::ffi::OsStr::new("temporary"))
            );
        }
        assert_eq!(std::env::var_os(RESTORE_GUARD_TEST_ENV), None);

        std::env::set_var(RESTORE_GUARD_TEST_ENV, "original");
        {
            let _removed_value = EnvVarRestoreGuard::remove(RESTORE_GUARD_TEST_ENV);
            assert_eq!(std::env::var_os(RESTORE_GUARD_TEST_ENV), None);
        }
        assert_eq!(
            std::env::var_os(RESTORE_GUARD_TEST_ENV).as_deref(),
            Some(std::ffi::OsStr::new("original"))
        );
    }
}
