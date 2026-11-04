//! Los iconos distribuidos deben cubrir toda la tecla y coincidir con el manifest.
use serde_json::Value;

#[test]
fn state_icons_are_opaque_96px_pngs_matching_the_manifest() {
    let manifest: Value = serde_json::from_str(include_str!("../manifest.json")).unwrap();
    for visual in manifest["visuals"].as_array().unwrap() {
        for state in visual["states"].as_array().unwrap() {
            let name = format!(
                "{}-{}",
                visual["id"].as_str().unwrap(),
                state["id"].as_str().unwrap()
            );
            let hex = state["icon"].as_str().unwrap();
            assert!(hex.len() <= 16384, "{name}: icon exceeds 8 KiB");
            assert_eq!(hex.len() % 2, 0, "{name}: incomplete hex byte");
            let bytes: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|offset| u8::from_str_radix(&hex[offset..offset + 2], 16).unwrap())
                .collect();
            assert_eq!(
                bytes,
                std::fs::read(format!("{}/icons/{name}.png", env!("CARGO_MANIFEST_DIR"))).unwrap(),
                "{name}: embedded bytes differ"
            );
            let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                .unwrap()
                .to_rgba8();
            assert_eq!(image.dimensions(), (96, 96), "{name}: dimensions");
            assert!(
                image.pixels().all(|pixel| pixel[3] == 255),
                "{name}: transparent pixels"
            );
        }
    }
}
