use sqlr_core::{driver_property_fields, validate_driver_properties};
#[test]
fn driver_property_validation() {
    let fields = driver_property_fields("sqlite");
    assert!(
        validate_driver_properties(
            &fields,
            &[("property.busy_timeout".into(), "100".into())].into()
        )
        .is_ok()
    );
    assert!(
        validate_driver_properties(
            &fields,
            &[("property.busy_timeout".into(), "-1".into())].into()
        )
        .is_err()
    );
    assert!(
        validate_driver_properties(&fields, &[("property.unknown".into(), "1".into())].into())
            .is_err()
    );
    assert!(
        validate_driver_properties(
            &fields,
            &[("property.foreign_keys".into(), "maybe".into())].into()
        )
        .is_err()
    );
    assert!(
        validate_driver_properties(
            &fields,
            &[("property.journal_mode".into(), "WAL; DROP TABLE x".into())].into()
        )
        .is_err()
    );
}
