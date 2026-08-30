//! Payment provider abstraction.

/// External payment integration handle.
pub trait PaymentProvider {
    fn enabled(&self) -> bool;
}

/// The no-op provider used while payments are disabled.
pub struct DisabledProvider;

impl PaymentProvider for DisabledProvider {
    fn enabled(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_provider_is_not_enabled() {
        assert!(!DisabledProvider.enabled());
    }
}
