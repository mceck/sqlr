#[test]
fn window_icon_does_not_allocate_full_resolution_native_surfaces() {
    let icon = sqlr_app::window_icon().unwrap();
    assert!(icon.width <= 256 && icon.height <= 256);
    assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
    assert!(icon.rgba.as_chunks::<4>().0.iter().any(|px| px[3] > 0));
}
