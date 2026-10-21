use super::*;
use serde_json::json;

fn catalog() -> Catalog {
    let entry = Entry::new(
        "user.test",
        Definition::Appearance(ScalarLook { name: "Warm oak colour".into(), color: [180, 120, 70], opacity: 0.8, pbr: None, pattern: None }),
        Provenance::default(),
    )
    .unwrap();
    Catalog { format: FORMAT.into(), id: "user.test".into(), name: "Original test library".into(), entries: vec![entry] }
}
#[test]
fn catalog_roundtrip_is_deterministic_and_revision_covers_provenance_and_definition() {
    let original = catalog();
    let bytes = original.encode().unwrap();
    let restored = Catalog::decode(&bytes).unwrap();
    assert_eq!(original, restored);
    assert_eq!(bytes, restored.encode().unwrap());
    let mut changed = restored.clone();
    changed.entries[0].provenance.license = "CC0-1.0".into();
    assert!(changed.validate().is_err());
    changed.entries[0].revision = changed.entries[0].content_revision().unwrap();
    assert_ne!(changed.entries[0].revision, original.entries[0].revision);
    changed.validate().unwrap();
    if let Definition::Appearance(look) = &mut changed.entries[0].definition {
        look.color = [1, 2, 3];
    }
    assert!(changed.validate().is_err());
}
#[test]
fn strict_schema_rejects_unknown_kinds_fields_versions_and_bad_identity() {
    let value = serde_json::to_value(catalog()).unwrap();
    for changed in [
        {
            let mut v = value.clone();
            // 2 is the fork's richer format (FORMAT_V2); 3 is unknown here as 2 is upstream.
            v["format"] = json!("solidcraft-library/3");
            v
        },
        {
            let mut v = value.clone();
            v["entries"][0]["definition"]["kind"] = json!("texture_set");
            v
        },
        {
            let mut v = value.clone();
            v["entries"][0]["definition"]["value"]["roughness"] = json!(0.5);
            v
        },
        {
            let mut v = value.clone();
            v["entries"][0]["id"] = json!("other.pack/appearance-a");
            v
        },
        {
            let mut v = value.clone();
            v["entries"][0]["revision"] = json!("0".repeat(64));
            v
        },
        {
            let mut v = value.clone();
            v["id"] = json!("USER.TEST");
            v
        },
        {
            let mut v = value.clone();
            v["unknown"] = json!(true);
            v
        },
    ] {
        assert!(Catalog::decode(&serde_json::to_vec(&changed).unwrap()).is_err());
    }
}
#[test]
fn scalar_validation_refuses_nonfinite_density_opacity_counts_strings_and_oversized_input() {
    let mut invalid = catalog();
    invalid.entries.push(invalid.entries[0].clone());
    assert!(invalid.validate().is_err());
    invalid.entries = vec![catalog().entries[0].clone(); MAX_ENTRIES + 1];
    assert!(invalid.validate().is_err());
    assert!(Catalog::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
    let entry = catalog().entries.remove(0);
    let mut boundary = catalog();
    boundary.entries = (0..MAX_ENTRIES).map(|i| Entry { id: format!("user.test/entry-{i}"), ..entry.clone() }).collect();
    Catalog::decode(&boundary.encode().unwrap()).unwrap();
    boundary.entries.push(Entry { id: "user.test/one-more".into(), ..entry });
    assert!(Catalog::decode(&serde_json::to_vec(&boundary).unwrap()).is_err());
    for opacity in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        let mut entry = catalog().entries.remove(0);
        if let Definition::Appearance(look) = &mut entry.definition {
            look.opacity = opacity;
        }
        assert!(entry.content_revision().is_err());
    }
    for density in [0.0, -1.0, 1000.1, f64::NAN, f64::INFINITY] {
        assert!(
            Entry::new(
                "user.test",
                Definition::Material(ScalarMaterial { name: "Timber".into(), density_g_cm3: density, appearance: None }),
                Provenance::default()
            )
            .is_err()
        );
    }
    for name in ["".into(), "x".repeat(129), "bad\nname".into(), " leading".into()] {
        assert!(
            Entry::new(
                "user.test",
                Definition::Appearance(ScalarLook { name, color: [1, 2, 3], opacity: 1.0, pbr: None, pattern: None }),
                Provenance::default()
            )
            .is_err()
        );
    }
    assert!(Catalog::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
    let source = Provenance { source_url: Some("x".repeat(2049)), ..Default::default() };
    assert!(Entry::new("user.test", catalog().entries[0].definition.clone(), source).is_err());
}
#[test]
fn design_export_deduplicates_exact_looks_and_keeps_physical_properties_distinct() {
    let mut doc = Document::default();
    let look = ScalarLook { name: "Warm oak colour".into(), color: [180, 120, 70], opacity: 0.8, pbr: None, pattern: None }.to_look();
    doc.appearances.bodies.insert("Body1".into(), look.clone());
    doc.appearances.bodies.insert("Body2".into(), look.clone());
    doc.user_materials.push(UserMaterial { name: "Timber density".into(), density: 0.7, appearance: Some(look) });
    let library = Catalog::from_document(&doc, "user.test", "Test", Provenance::default()).unwrap();
    assert_eq!(library.entries.len(), 2);
    assert!(library.entries.iter().any(|e| matches!(&e.definition,Definition::Material(m) if m.density_g_cm3==0.7)));
    assert_eq!(doc.user_materials[0].density, 0.7);
    assert_eq!(library, Catalog::decode(&library.encode().unwrap()).unwrap());
    assert!(Catalog::from_document(&Document::default(), "user.test", "Test", Provenance { license: String::new(), ..Default::default() }).is_err());
}

#[test]
fn rich_looks_export_as_v2_from_every_design_owned_appearance_location() {
    // Upstream's v1 refuses a PBR look rather than drop it; the fork's v2 keeps it (and a
    // pattern) instead, so the export is still never lossy.
    use solidcraft_doc::{
        appearance::FaceLook,
        pattern_appearance::{Pattern, PatternAxis, PatternKind},
        pbr_appearance::PbrParameters,
    };
    let mut look = ScalarLook { name: "Original PBR specimen".into(), color: [180, 120, 70], opacity: 1.0, pbr: None, pattern: None }.to_look();
    look.pbr = Some(PbrParameters::new(1.0, 0.3, 1.5).unwrap());
    look.pattern = Some(Pattern::new(PatternKind::Wood, 4.0, 0.7, [120, 80, 50], PatternAxis::Auto, 0).unwrap());
    for location in 0..4 {
        let mut doc = Document::default();
        match location {
            0 => {
                doc.appearances.bodies.insert("Specimen".into(), look.clone());
            }
            1 => {
                doc.appearances.components.insert(1, look.clone());
            }
            2 => doc.appearances.faces.push(FaceLook {
                body: "Specimen".into(),
                point: Default::default(),
                look: look.clone(),
                name: None,
                strict_name: false,
                certificate: None,
            }),
            _ => doc.user_materials.push(UserMaterial { name: "Original material".into(), density: 1.2, appearance: Some(look.clone()) }),
        }
        let before = doc.clone();
        let library = Catalog::from_document(&doc, "user.test", "Test", Provenance::default()).unwrap();
        assert_eq!(library.format, FORMAT_V2);
        let reread = Catalog::decode(&library.encode().unwrap()).unwrap();
        assert_eq!(reread, library);
        let looks: Vec<Look> = reread
            .entries
            .iter()
            .filter_map(|e| match &e.definition {
                Definition::Appearance(l) => Some(l.to_look()),
                Definition::Material(m) => m.appearance.as_ref().map(ScalarLook::to_look),
            })
            .collect();
        assert!(looks.iter().all(|l| l.pbr == look.pbr && l.pattern == look.pattern), "{looks:?}");
        assert_eq!(doc, before);
    }
    // A plain library stays v1, and a v1 file may not smuggle in the richer fields.
    let plain = Catalog::from_document(&Document::default(), "user.test", "Test", Provenance::default()).unwrap();
    assert_eq!(plain.format, FORMAT);
    let mut doc = Document::default();
    doc.appearances.bodies.insert("Specimen".into(), look.clone());
    let mut rich = Catalog::from_document(&doc, "user.test", "Test", Provenance::default()).unwrap();
    rich.format = FORMAT.into();
    assert!(rich.validate().is_err());
}

/// A hand-written library (a self-hosting patch): entries without `id`, `revision`,
/// `provenance` or `opacity` get them on reading, exactly as `Entry::new` would; a stated
/// revision that doesn't match is still refused.
#[test]
fn hand_written_entries_get_their_identity_and_revision_on_reading() {
    let hand = json!({
        "format": "solidcraft-library/2",
        "id": "my.stones",
        "name": "My stones",
        "entries": [
            {"definition": {"kind": "appearance", "value": {"name": "Stone - Grey granite", "color": [128, 128, 130],
                "pattern": {"kind": "speckled", "scale_mm": 2.5, "contrast": 0.5, "color": [40, 40, 44]}}}},
            {"definition": {"kind": "material", "value": {"name": "Granite", "density_g_cm3": 2.7,
                "appearance": {"name": "Stone - Grey granite", "color": [128, 128, 130]}}}}
        ]
    });
    let library = Catalog::decode(&serde_json::to_vec(&hand).unwrap()).unwrap();
    assert_eq!(library.entries.len(), 2);
    for entry in &library.entries {
        let made = Entry::new("my.stones", entry.definition.clone(), Provenance::default()).unwrap();
        assert_eq!((&entry.id, &entry.revision, &entry.provenance), (&made.id, &made.revision, &made.provenance));
    }
    assert_eq!(Catalog::decode(&library.encode().unwrap()).unwrap(), library);
    let mut wrong = hand.clone();
    wrong["entries"][0]["revision"] = json!("0".repeat(64));
    assert!(Catalog::decode(&serde_json::to_vec(&wrong).unwrap()).is_err());
    let mut v1 = hand.clone();
    v1["format"] = json!("solidcraft-library/1");
    assert!(Catalog::decode(&serde_json::to_vec(&v1).unwrap()).is_err(), "a v1 file can't carry a pattern");
}
