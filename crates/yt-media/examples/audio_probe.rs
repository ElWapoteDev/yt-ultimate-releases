//! Plays generated digital silence to check the system audio device safely.
use std::{io::Write, thread, time::Duration};
use yt_media::AudioPlayer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut file = tempfile::Builder::new().suffix(".wav").tempfile()?;
    let samples = 8000u32 * 3;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + samples * 2).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&8000u32.to_le_bytes())?;
    file.write_all(&16000u32.to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&(samples * 2).to_le_bytes())?;
    file.write_all(&vec![0u8; samples as usize * 2])?;
    file.flush()?;
    let player = AudioPlayer::new()?;
    player.play(file.path())?;
    thread::sleep(Duration::from_millis(250));
    let snapshot = player.snapshot();
    assert!(snapshot.is_playing);
    assert!(snapshot.position_seconds > 0.0);
    player.pause()?;
    thread::sleep(Duration::from_millis(150));
    assert!(!player.snapshot().is_playing);
    player.seek(1.0)?;
    player.set_volume(0.25)?;
    player.set_speed(1.25)?;
    player.resume()?;
    thread::sleep(Duration::from_millis(150));
    assert!(player.snapshot().is_playing);
    player.set_sleep_timer(Some(Duration::from_millis(100)))?;
    thread::sleep(Duration::from_millis(250));
    assert!(!player.snapshot().is_playing);
    player.stop()?;
    println!("Native audio: playback, pause, seek, volume, speed and sleep timer passed.");
    Ok(())
}
