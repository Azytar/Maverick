//! Strategies shared by the root crate's property tests, never production.

use proptest::prelude::*;

/// Every representable value in a finite, strictly positive f32 interval.
/// Positive IEEE-754 encodings are ordered, so sampling their integer encodings
/// preserves the complete domain and endpoints without floating arithmetic.
/// Proptest 1.11's float sampler can panic while rounding a decimal lower bound.
pub(crate) fn positive_f32(low: f32, high: f32) -> impl Strategy<Value = f32> {
    assert!(low.is_finite() && high.is_finite() && 0.0 < low && low <= high);
    (low.to_bits()..=high.to_bits()).prop_map(f32::from_bits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

    #[test]
    fn decimal_lower_bound_survives_the_upstream_sampler_reproducer() {
        // This exact RNG stream panics inside (0.05f32..=1.0).new_tree()
        // with Proptest 1.11, before a Maverick assertion can be evaluated.
        let rng = TestRng::from_seed(RngAlgorithm::PassThrough, &[0xff; 8]);
        let mut runner = TestRunner::new_with_rng(Config::default(), rng);
        let value = positive_f32(0.05, 1.0)
            .new_tree(&mut runner)
            .unwrap()
            .current();
        assert!((0.05..=1.0).contains(&value));
    }

    #[test]
    fn singleton_endpoints_are_exact() {
        let mut runner = TestRunner::deterministic();
        for endpoint in [0.05, 0.1, 1.0, 2.0, 4.0] {
            let value = positive_f32(endpoint, endpoint)
                .new_tree(&mut runner)
                .unwrap()
                .current();
            assert_eq!(value.to_bits(), endpoint.to_bits());
        }
    }

    proptest! {
        #[test]
        fn generated_weights_are_finite_and_in_range(value in positive_f32(0.05, 1.0)) {
            prop_assert!(value.is_finite() && (0.05..=1.0).contains(&value));
        }
    }
}
