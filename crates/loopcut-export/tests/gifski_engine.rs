//! Integration tests for the gifski engine. Each test writes a real GIF and decodes it with the
//! `gif` 0.14 crate, which is independent of the gif 0.13 that gifski itself uses.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use loopcut_core::Rational;
use loopcut_core::gif_timing::delays_cs;
use loopcut_export::gif::{
    GifError, GifProgress, GifStats, GifskiEncoder, GifskiOptions, pts_for_exact_delays,
};

/// Upper bound for any single encode in these tests. A hang fails the test instead of stalling CI.
const TIMEOUT: Duration = Duration::from_secs(120);

struct Decoded {
    width: u16,
    height: u16,
    repeat: gif::Repeat,
    frames: Vec<FrameInfo>,
}

struct FrameInfo {
    delay: u16,
    width: u16,
    height: u16,
}

fn decode(path: &Path) -> Decoded {
    let file = File::open(path).expect("open GIF");
    let mut decoder = gif::DecodeOptions::new()
        .read_info(file)
        .expect("read GIF header");
    let width = decoder.width();
    let height = decoder.height();
    let repeat = decoder.repeat();
    let mut frames = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("decode GIF frame") {
        frames.push(FrameInfo {
            delay: frame.delay,
            width: frame.width,
            height: frame.height,
        });
    }
    Decoded {
        width,
        height,
        repeat,
        frames,
    }
}

/// True when `bytes` decodes all the way to the GIF trailer. A stream that ends early fails with
/// `UnexpectedEof` in gif 0.14, so this is false for any truncated output.
fn reads_to_trailer(bytes: &[u8]) -> bool {
    let Ok(mut decoder) = gif::DecodeOptions::new().read_info(bytes) else {
        return false;
    };
    loop {
        match decoder.read_next_frame() {
            Ok(Some(_)) => {}
            Ok(None) => return true,
            Err(_) => return false,
        }
    }
}

/// Runs `f` on a helper thread and fails the test if it does not finish within [`TIMEOUT`].
fn guard<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(TIMEOUT) {
        Ok(value) => value,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what}: no result within {TIMEOUT:?} (deadlock?)")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what}: helper thread panicked"),
    }
}

/// Distinct frame at every `t`: every pixel changes, so gifski cannot merge or drop frames.
fn moving_gradient(width: u32, height: u32, t: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let r = (x * 4 + t * 37) % 256;
            let g = (y * 5 + t * 11) % 256;
            let b = (x + y + t * 23) % 256;
            px.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
    }
    px
}

/// Opaque xorshift noise, different for every seed.
fn noise_frame(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let mut state = (seed ^ 0x9E37_79B9) | 1;
    let mut out = vec![0u8; (width * height * 4) as usize];
    for pixel in out.as_chunks_mut::<4>().0 {
        for byte in &mut pixel[..3] {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = (state >> 24) as u8;
        }
        pixel[3] = 255;
    }
    out
}

/// A moving gradient with `amp`/255 of per-pixel noise on top. Structured enough that gifski keeps
/// every frame, unlike pure noise (see `pure_noise_may_drop_frames_but_keeps_total_duration`).
fn noisy_gradient(width: u32, height: u32, t: u32, amp: u32) -> Vec<u8> {
    let mut out = noise_frame(width, height, t * 7919 + 1);
    for (i, pixel) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = i as u32 % width;
        let y = i as u32 / width;
        let base = [(x * 3 + t * 20) % 256, (y * 4) % 256, (x + y + t * 9) % 256];
        for (c, &b) in base.iter().enumerate() {
            let n = u32::from(pixel[c]) * amp / 255;
            pixel[c] = ((b + n) % 256) as u8;
        }
    }
    out
}

/// A natural-looking clip: a sky-like gradient, two discs and a bar moving across it, and mild
/// per-pixel grain of about +/-8 levels, like camera noise.
fn natural_clip_frame(width: u32, height: u32, t: u32) -> Vec<u8> {
    let (w, h) = (i64::from(width), i64::from(height));
    let tt = i64::from(t);
    let disc_a = ((tt * 7) % w, h / 3);
    let disc_b = (w - (tt * 5) % w, (h * 2) / 3);
    let radius = h / 8;
    let bar_top = h * 5 / 6 - h / 20;
    let bar_bottom = h * 5 / 6 + h / 20;
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let mut rgb = [80 + x * 120 / w, 110 + y * 90 / h, 200 - y * 100 / h];
            let inside =
                |(cx, cy): (i64, i64)| (x - cx).pow(2) + (y - cy).pow(2) <= radius * radius;
            if inside(disc_a) {
                rgb = [230, 120, 40];
            } else if inside(disc_b) {
                rgb = [40, 170, 150];
            }
            if (bar_top..=bar_bottom).contains(&y) && (x + tt * 4) % w < w / 4 {
                rgb = [250, 240, 220];
            }
            // Grain: a small integer hash of the pixel position and frame, mapped to -8..=7.
            let mut hash = (x as u32).wrapping_mul(0x9E37_79B1)
                ^ (y as u32).wrapping_mul(0x85EB_CA77)
                ^ (t).wrapping_mul(0xC2B2_AE3D);
            hash ^= hash >> 15;
            hash = hash.wrapping_mul(0x2C1B_3C6D);
            let grain = i64::from(hash >> 28) - 8;
            for channel in rgb {
                out.push((channel + grain).clamp(0, 255) as u8);
            }
            out.push(255);
        }
    }
    out
}

fn options(width: u32, height: u32) -> GifskiOptions {
    GifskiOptions {
        width,
        height,
        ..GifskiOptions::default()
    }
}

/// Encodes `frames` at timestamps `pts` into `path`, feeding frames from a helper thread.
fn encode(path: PathBuf, opts: GifskiOptions, frames: Vec<Vec<u8>>, pts: Vec<f64>) -> GifStats {
    guard("encode", move || {
        let file = File::create(&path).expect("create GIF");
        let enc = GifskiEncoder::start(&opts, file, Box::new(|_| true)).expect("start encoder");
        for (index, (frame, t)) in frames.into_iter().zip(pts).enumerate() {
            enc.add_frame(index, frame, opts.width, opts.height, t)
                .expect("add frame");
        }
        enc.finish().expect("finish encode")
    })
}

/// Output that shares its bytes with the test, so the test can read what the writer produced.
#[derive(Clone, Default)]
struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

impl SharedBuffer {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().expect("lock buffer").clone()
    }
}

impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("lock buffer").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The timeline used before `pts_for_exact_delays` existed: frame `i` starts when the delays before
/// it have elapsed, with no offset. gifski gives the last frame the previous interval, so this
/// layout does not reproduce the last frame's own delay.
fn naive_pts(delays_cs: &[u16]) -> Vec<f64> {
    let mut start_cs: u64 = 0;
    delays_cs
        .iter()
        .map(|&delay| {
            let pts = start_cs as f64 / 100.0;
            start_cs += u64::from(delay);
            pts
        })
        .collect()
}

#[test]
fn thirty_frames_keep_size_delays_and_loop_forever() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("loop.gif");
    let delays = delays_cs(30, Rational::integer(20));
    assert!(delays.iter().all(|&d| d == 5), "20 fps is 5 cs per frame");

    let frames = (0..30).map(|t| moving_gradient(64, 48, t)).collect();
    let stats = encode(
        path.clone(),
        options(64, 48),
        frames,
        pts_for_exact_delays(&delays),
    );

    let decoded = decode(&path);
    assert_eq!((decoded.width, decoded.height), (64, 48));
    assert_eq!(
        decoded.frames.len(),
        30,
        "gifski must keep all 30 distinct frames"
    );
    assert_eq!(stats.frames, decoded.frames.len());
    assert!(
        decoded.frames.iter().all(|f| f.delay == 5),
        "every delay must be 5 cs"
    );
    assert!(
        decoded
            .frames
            .iter()
            .all(|f| (f.width, f.height) == (64, 48))
    );
    assert_eq!(decoded.repeat, gif::Repeat::Infinite);
    assert_eq!(fs::metadata(&path).expect("stat").len(), stats.bytes);
}

#[test]
fn loop_count_follows_the_project_model() {
    let dir = tempfile::tempdir().expect("tempdir");
    let frames = || {
        (0..4)
            .map(|t| moving_gradient(32, 32, t))
            .collect::<Vec<_>>()
    };
    let pts = pts_for_exact_delays(&[5, 5, 5, 5]);
    let run = |name: &str, loop_count: u16| {
        let mut opts = options(32, 32);
        opts.loop_count = loop_count;
        let path = dir.path().join(name);
        encode(path.clone(), opts, frames(), pts.clone());
        path
    };

    // 0 loops forever: NETSCAPE repeat count 0.
    let forever = run("forever.gif", 0);
    assert_eq!(decode(&forever).repeat, gif::Repeat::Infinite);

    // 1 plays once: gifski writes no NETSCAPE block at all. gif 0.14 reports Finite(0) when the
    // block is absent, so check the bytes too, because a NETSCAPE block would mean looping.
    let once = run("once.gif", 1);
    assert_eq!(decode(&once).repeat, gif::Repeat::Finite(0));
    let once_bytes = fs::read(&once).expect("read GIF");
    assert!(
        !once_bytes.windows(11).any(|w| w == b"NETSCAPE2.0"),
        "a single play must not write a NETSCAPE loop block"
    );

    // 3 plays in total is two repeats.
    let three = run("three.gif", 3);
    assert_eq!(decode(&three).repeat, gif::Repeat::Finite(2));
}

#[test]
fn single_frame_keeps_its_own_delay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("single.gif");
    encode(
        path.clone(),
        options(64, 48),
        vec![moving_gradient(64, 48, 0)],
        pts_for_exact_delays(&[5]),
    );
    let decoded = decode(&path);
    assert_eq!(decoded.frames.len(), 1);
    assert_eq!(
        decoded.frames[0].delay, 5,
        "a single 5 cs frame must keep its 5 cs delay"
    );

    // The old timeline gives the lone frame gifski's 2 cs floor, which is why it was removed.
    let naive_path = dir.path().join("single-naive.gif");
    encode(
        naive_path.clone(),
        options(64, 48),
        vec![moving_gradient(64, 48, 0)],
        naive_pts(&[5]),
    );
    assert_eq!(decode(&naive_path).frames[0].delay, 2);
}

#[test]
fn finish_without_frames_is_no_frames() {
    let result = guard("empty encode", || {
        let file = tempfile::tempfile().expect("tempfile");
        let enc = GifskiEncoder::start(&options(16, 16), file, Box::new(|_| true))
            .expect("start encoder");
        enc.finish()
    });
    assert!(
        matches!(result, Err(GifError::NoFrames)),
        "finish() with no frames must return NoFrames, got {result:?}"
    );
}

#[test]
fn non_uniform_delays_with_exact_pts_reproduce_every_delay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let delays = delays_cs(15, Rational::integer(15));
    let n = delays.len();
    assert_ne!(
        delays[n - 2],
        delays[n - 1],
        "need the last two delays to differ"
    );

    let exact_path = dir.path().join("exact.gif");
    let frames = (0..15).map(|t| moving_gradient(48, 36, t)).collect();
    encode(
        exact_path.clone(),
        options(48, 36),
        frames,
        pts_for_exact_delays(&delays),
    );
    let exact = decode(&exact_path);
    let got: Vec<u16> = exact.frames.iter().map(|f| f.delay).collect();
    assert_eq!(
        got, delays,
        "pts_for_exact_delays must reproduce delays exactly"
    );
    assert_eq!(
        got.iter().map(|&d| u32::from(d)).sum::<u32>(),
        100,
        "15 fps loop lasts exactly 1 s"
    );

    // Documents the gifski quirk that the old timeline had: the last frame's delay is the previous
    // interval, not its own.
    let naive_path = dir.path().join("naive.gif");
    let frames = (0..15).map(|t| moving_gradient(48, 36, t)).collect();
    encode(
        naive_path.clone(),
        options(48, 36),
        frames,
        naive_pts(&delays),
    );
    let naive = decode(&naive_path);
    let got_naive: Vec<u16> = naive.frames.iter().map(|f| f.delay).collect();
    assert_eq!(
        got_naive[n - 1],
        delays[n - 2],
        "the naive timeline gives the last frame the previous interval (gifski's rule)"
    );
    assert_eq!(got_naive[..n - 1], delays[..n - 1]);
}

#[test]
fn pure_noise_may_drop_frames_but_keeps_total_duration() {
    // gifski's denoiser treats per-pixel noise as unimportant and can drop frames, merging their
    // time into the neighbouring frame. The loop length must still be exact.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("noise.gif");
    let frames = (0..8).map(|seed| noise_frame(160, 120, seed)).collect();
    let stats = encode(
        path.clone(),
        options(160, 120),
        frames,
        pts_for_exact_delays(&[5; 8]),
    );
    let decoded = decode(&path);
    assert_eq!(stats.frames, decoded.frames.len());
    assert!(decoded.frames.len() <= 8);
    let total: u32 = decoded.frames.iter().map(|f| u32::from(f.delay)).sum();
    assert_eq!(total, 40, "8 frames at 5 cs must still last 40 cs");
}

#[test]
fn lossy_below_100_runs_the_gifsicle_pass() {
    // Lossy values below 100 enable gifsicle's lossy LZW pass, which changes the output size and
    // not monotonically: on this noisy input 99 comes out larger than lossless, while 1 comes out
    // smaller. The test checks that the pass runs and that every noisy frame survives. The size
    // curve on natural-looking footage is printed by `lossy_size_curve_on_a_natural_clip`.
    let dir = tempfile::tempdir().expect("tempdir");
    let frames = || {
        (0..8)
            .map(|t| noisy_gradient(160, 120, t, 96))
            .collect::<Vec<_>>()
    };
    let pts = pts_for_exact_delays(&[5; 8]);

    let encode_with = |name: &str, lossy: u8| {
        let mut opts = options(160, 120);
        opts.lossy_quality = lossy;
        let path = dir.path().join(name);
        let stats = encode(path.clone(), opts, frames(), pts.clone());
        assert_eq!(
            decode(&path).frames.len(),
            8,
            "all 8 noisy frames must be kept"
        );
        stats.bytes
    };
    let lossless = encode_with("lossless.gif", 100);
    let just_below = encode_with("lossy99.gif", 99);
    let extreme = encode_with("lossy1.gif", 1);

    assert!(
        just_below > lossless,
        "lossy 99 gave {just_below} bytes, lossless gave {lossless}"
    );
    assert!(
        extreme < lossless,
        "lossy 1 gave {extreme} bytes, lossless gave {lossless}"
    );
}

#[test]
#[ignore = "measurement: run with --ignored --nocapture to print the lossy size curve"]
fn lossy_size_curve_on_a_natural_clip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let frame_count = 24u32;
    let frames = || {
        (0..frame_count)
            .map(|t| natural_clip_frame(320, 180, t))
            .collect::<Vec<_>>()
    };
    let pts = pts_for_exact_delays(&delays_cs(frame_count as usize, Rational::integer(20)));

    let mut lossless_bytes = None;
    for lossy in [100u8, 80, 50, 30, 10] {
        let mut opts = options(320, 180);
        opts.lossy_quality = lossy;
        let path = dir.path().join(format!("lossy{lossy}.gif"));
        let stats = encode(path.clone(), opts, frames(), pts.clone());
        let kept = decode(&path).frames.len();
        let base = *lossless_bytes.get_or_insert(stats.bytes);
        println!(
            "lossy {lossy:>3}: {:>8} bytes ({:>6.1}% of lossy 100), {kept} of {frame_count} frames kept",
            stats.bytes,
            stats.bytes as f64 * 100.0 / base as f64
        );
    }
}

#[test]
fn progress_callback_can_abort_promptly() {
    let outcome = guard("abort", || {
        let opts = options(64, 48);
        let file = tempfile::tempfile().expect("tempfile");
        let enc = GifskiEncoder::start(
            &opts,
            file,
            Box::new(|progress: GifProgress| progress.frames_written < 5),
        )
        .expect("start encoder");
        let mut accepted = 0;
        for t in 0..30u32 {
            let frame = moving_gradient(64, 48, t);
            if enc
                .add_frame(t as usize, frame, 64, 48, f64::from(t) * 0.05)
                .is_err()
            {
                break;
            }
            accepted += 1;
        }
        (accepted, enc.finish())
    });
    // `accepted` depends on pipeline timing, so it is not asserted; the guarantee is that
    // finish() returns instead of hanging, with the abort reported as Aborted.
    let (_accepted, result) = outcome;
    assert!(
        matches!(result, Err(GifError::Aborted)),
        "finish() after an abort must return Aborted, got {result:?}"
    );
}

#[test]
fn callback_abort_never_writes_the_trailer() {
    let buffer = SharedBuffer::default();
    let output = buffer.clone();
    let result = guard("abort", move || {
        let enc = GifskiEncoder::start(
            &options(64, 48),
            buffer,
            Box::new(|progress: GifProgress| progress.frames_written < 5),
        )
        .expect("start encoder");
        for t in 0..30u32 {
            let frame = moving_gradient(64, 48, t);
            if enc
                .add_frame(t as usize, frame, 64, 48, f64::from(t) * 0.05)
                .is_err()
            {
                break;
            }
        }
        enc.finish()
    });
    assert!(matches!(result, Err(GifError::Aborted)), "got {result:?}");
    assert!(
        !reads_to_trailer(&output.bytes()),
        "an aborted GIF must not decode to its trailer"
    );
}

#[test]
fn cancel_never_writes_the_trailer() {
    let buffer = SharedBuffer::default();
    let output = buffer.clone();
    guard("cancel", move || {
        let enc = GifskiEncoder::start(&options(64, 48), buffer, Box::new(|_| true))
            .expect("start encoder");
        for t in 0..10u32 {
            enc.add_frame(
                t as usize,
                moving_gradient(64, 48, t),
                64,
                48,
                f64::from(t) * 0.05,
            )
            .expect("add frame");
        }
        enc.cancel();
    });
    assert!(
        !reads_to_trailer(&output.bytes()),
        "a cancelled GIF must not decode to its trailer"
    );
}

#[test]
fn identical_frames_merge_and_keep_their_total_duration() {
    // gifski keeps the last frame of an identical run, and that frame's delay covers the whole run.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("still.gif");
    let frame = moving_gradient(40, 30, 7);
    let frames = (0..10).map(|_| frame.clone()).collect();
    let stats = encode(
        path.clone(),
        options(40, 30),
        frames,
        pts_for_exact_delays(&[5; 10]),
    );

    let decoded = decode(&path);
    assert_eq!(stats.frames, decoded.frames.len());
    assert_eq!(
        decoded.frames.len(),
        1,
        "ten identical frames collapse into one"
    );
    assert_eq!(
        decoded.frames[0].delay, 50,
        "the merged frame keeps all 0.5 s"
    );
}

#[test]
fn large_frames_are_not_downscaled() {
    // gifski's own default would halve a 1280x720 frame (it downscales above ~0.84 megapixels),
    // so this checks that the adapter passes the exact size.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hd.gif");
    let frames = (0..3).map(|t| moving_gradient(1280, 720, t)).collect();
    encode(
        path.clone(),
        options(1280, 720),
        frames,
        pts_for_exact_delays(&[5, 5, 5]),
    );
    let decoded = decode(&path);
    assert_eq!((decoded.width, decoded.height), (1280, 720));
}

#[test]
fn progress_is_monotonic_and_ends_at_the_frame_count() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let stats = guard("progress", move || {
        let opts = options(64, 48);
        let file = tempfile::tempfile().expect("tempfile");
        let enc = GifskiEncoder::start(
            &opts,
            file,
            Box::new(move |progress| {
                sink.lock().expect("lock").push(progress);
                true
            }),
        )
        .expect("start encoder");
        for t in 0..12u32 {
            enc.add_frame(
                t as usize,
                moving_gradient(64, 48, t),
                64,
                48,
                f64::from(t) * 0.05,
            )
            .expect("add frame");
        }
        enc.finish().expect("finish")
    });
    let seen = seen.lock().expect("lock");
    assert!(!seen.is_empty(), "callback must run");
    assert!(
        seen.windows(2)
            .all(|w| w[0].frames_written <= w[1].frames_written
                && w[0].bytes_written <= w[1].bytes_written)
    );
    let last = seen.last().expect("last");
    assert_eq!(last.frames_written, stats.frames as u64);
    // The last reported size excludes only the one-byte trailer.
    assert_eq!(stats.bytes, last.bytes_written + 1);
}

/// Accepts `limit` bytes and then fails every write, like a full disk.
struct FailingAfter {
    limit: usize,
    written: usize,
}

impl Write for FailingAfter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written >= self.limit {
            return Err(std::io::Error::other("disk full"));
        }
        let n = buf.len().min(self.limit - self.written);
        self.written += n;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn failing_output_is_reported_even_for_the_trailer_byte() {
    let dir = tempfile::tempdir().expect("tempdir");
    let frames = || {
        (0..30)
            .map(|t| moving_gradient(64, 48, t))
            .collect::<Vec<_>>()
    };
    let pts = pts_for_exact_delays(&[5; 30]);
    let total = encode(
        dir.path().join("reference.gif"),
        options(64, 48),
        frames(),
        pts.clone(),
    )
    .bytes;

    // Every byte except the final trailer is accepted, so only the last write fails.
    let limit = usize::try_from(total - 1).expect("size fits usize");
    let result = guard("failing sink", move || {
        let sink = FailingAfter { limit, written: 0 };
        let enc = GifskiEncoder::start(&options(64, 48), sink, Box::new(|_| true))
            .expect("start encoder");
        for (index, (frame, t)) in frames().into_iter().zip(pts).enumerate() {
            enc.add_frame(index, frame, 64, 48, t).expect("add frame");
        }
        enc.finish()
    });
    assert!(
        matches!(result, Err(GifError::Io(_))),
        "a failed final write must not look like success, got {result:?}"
    );
}

#[test]
#[ignore = "benchmark: run explicitly with --ignored --nocapture"]
fn encode_speed_480x270_100_frames_quality_90() {
    // Run explicitly: cargo test -p loopcut-export --test gifski_engine -- --ignored --nocapture
    let dir = tempfile::tempdir().expect("tempdir");
    for fast in [false, true] {
        let path = dir.path().join(format!("speed-{fast}.gif"));
        let mut opts = options(480, 270);
        opts.fast = fast;
        let frames: Vec<Vec<u8>> = (0..100).map(|t| moving_gradient(480, 270, t)).collect();
        let pts = pts_for_exact_delays(&[5; 100]);
        let started = Instant::now();
        let stats = encode(path.clone(), opts, frames, pts);
        let secs = started.elapsed().as_secs_f64();
        println!(
            "gifski 480x270 x100 q90 fast={fast}: {secs:.2}s, {:.1} frames/s, {} GIF frames, {} bytes",
            100.0 / secs,
            stats.frames,
            stats.bytes
        );
    }
}
