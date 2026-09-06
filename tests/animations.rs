/*
File: crates/ag-psd/tests/animations.rs

Purpose:
End-to-end coverage for the frame-animation image resource (id 4000, the
`mani`/`IRFR` payload) through the public document pipeline.

Main responsibilities:
- prove that `ImageResources::animations` survives `write_psd` -> `read_psd`;
- pin the on-disk framing our writer emits against what Photoshop writes:
  nested frame / animation-set descriptors carry the classID "null" and the `AnDs`
  block is followed by an empty `Roll` block;
- cover a payload whose length is not a multiple of four, which no Photoshop
  fixture does (see the padding note in `image_resources::write_animations`).

Notes:
Self-contained: the document is synthesized here, so the test needs none of the
upstream fixture corpus (which is absent from the published package).
*/

use ag_psd::psd::{
    AnimationDispose, AnimationFrameInfo, AnimationInfo, Animations, ImageResources, Psd,
    ReadOptions, WriteOptions,
};
use ag_psd::reader::read_psd;
use ag_psd::writer::write_psd;

/// Number of non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack.windows(needle.len()).filter(|w| *w == needle).count()
}

/// A 2x2 document carrying two animation frames and one animation set.
fn document_with_animations() -> Psd {
    Psd {
        width: 2.0,
        height: 2.0,
        image_resources: Some(ImageResources {
            animations: Some(Animations {
                frames: vec![
                    AnimationFrameInfo {
                        id: 393_816_367.0,
                        delay: 0.3,
                        dispose: Some(AnimationDispose::Auto),
                    },
                    AnimationFrameInfo {
                        id: 393_833_174.0,
                        delay: 0.3,
                        dispose: Some(AnimationDispose::Dispose),
                    },
                ],
                animations: vec![AnimationInfo {
                    id: 0.0,
                    frames: vec![393_816_367.0, 393_833_174.0],
                    repeats: Some(2.0),
                    active_frame: Some(1.0),
                }],
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn animations_survive_a_document_round_trip() {
    let bytes = write_psd(&document_with_animations(), &WriteOptions::default());
    let psd = read_psd(&bytes, &ReadOptions::default()).expect("read_psd");

    let resources = psd.image_resources.expect("image resources");
    let animations = resources.animations.expect("animations");
    assert_eq!(animations.frames.len(), 2);
    assert_eq!(animations.frames[0].id, 393_816_367.0);
    assert!((animations.frames[0].delay - 0.3).abs() < 1e-9);
    // `FrDs` is omitted for automatic frames, and a missing key reads as `auto`.
    assert_eq!(animations.frames[0].dispose, Some(AnimationDispose::Auto));
    assert_eq!(animations.frames[1].dispose, Some(AnimationDispose::Dispose));
    assert_eq!(animations.animations.len(), 1);
    assert_eq!(animations.animations[0].frames, vec![393_816_367.0, 393_833_174.0]);
    assert_eq!(animations.animations[0].repeats, Some(2.0));
    assert_eq!(animations.animations[0].active_frame, Some(1.0));
}

/// Length of the `AnDs` block in a written document, i.e. the payload the
/// animation descriptor is serialized into.
fn ands_block_length(bytes: &[u8]) -> usize {
    let key = bytes
        .windows(4)
        .position(|w| w == b"AnDs")
        .expect("the document must contain an AnDs block");
    let start = key + 4;
    let length: [u8; 4] = bytes[start..start + 4]
        .try_into()
        .expect("four length bytes follow the AnDs key");
    u32::from_be_bytes(length) as usize
}

/// A document whose animation descriptor tree holds an even number of frame and
/// animation-set descriptors, so the `AnDs` payload is *not* a multiple of four
/// bytes long.
///
/// Every Photoshop fixture we have holds an odd number, which is exactly the
/// case where an unpadded and a padded-to-four encoding coincide, so this is the
/// one shape the corpus leaves untested. The assertions below therefore only
/// state that our own reader accepts what our own writer produces — they make no
/// claim about what Photoshop would write here.
fn document_with_odd_descriptor_count() -> Psd {
    Psd {
        width: 2.0,
        height: 2.0,
        image_resources: Some(ImageResources {
            animations: Some(Animations {
                frames: vec![AnimationFrameInfo {
                    id: 7.0,
                    delay: 0.1,
                    dispose: Some(AnimationDispose::None),
                }],
                animations: vec![AnimationInfo {
                    id: 0.0,
                    frames: vec![7.0],
                    repeats: Some(0.0),
                    active_frame: Some(0.0),
                }],
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn an_unaligned_animation_payload_round_trips() {
    let bytes = write_psd(&document_with_odd_descriptor_count(), &WriteOptions::default());

    // One frame + one animation set + the root = three descriptors, each
    // contributing an 18-byte header, so the payload length is 2 (mod 4). The
    // assertion exists so that this test cannot silently drift into exercising
    // the aligned case again.
    assert_eq!(ands_block_length(&bytes) % 4, 2);

    let psd = read_psd(&bytes, &ReadOptions::default()).expect("read_psd");
    let animations = psd
        .image_resources
        .expect("image resources")
        .animations
        .expect("animations");
    assert_eq!(animations.frames.len(), 1);
    assert_eq!(animations.frames[0].id, 7.0);
    assert_eq!(animations.frames[0].dispose, Some(AnimationDispose::None));
    assert_eq!(animations.animations.len(), 1);
    assert_eq!(animations.animations[0].frames, vec![7.0]);
}

#[test]
fn animation_resource_uses_photoshop_framing() {
    let bytes = write_psd(&document_with_animations(), &WriteOptions::default());

    assert_eq!(count_occurrences(&bytes, b"mani"), 1);
    assert_eq!(count_occurrences(&bytes, b"IRFR"), 1);
    assert_eq!(count_occurrences(&bytes, b"AnDs"), 1);
    // Photoshop closes every animation resource with an empty `Roll` block.
    assert_eq!(count_occurrences(&bytes, b"Roll"), 1);
    // The nested descriptors are `nullType`; "AnFr"/"AnSt" were a porting mistake.
    assert_eq!(count_occurrences(&bytes, b"AnFr"), 0);
    assert_eq!(count_occurrences(&bytes, b"AnSt"), 0);
}
