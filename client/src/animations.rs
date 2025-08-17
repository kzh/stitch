use console::Term;
use owo_colors::OwoColorize;
use std::io::{self, Write};
use std::time::Duration;
use tokio::time::sleep;
use unicode_width::UnicodeWidthStr;

fn center_text(text: &str, term_width: usize) -> String {
    text.lines()
        .map(|line| {
            let line_width = UnicodeWidthStr::width(line);
            if line_width >= term_width {
                line.to_string()
            } else {
                let padding = (term_width.saturating_sub(line_width)) / 2;
                format!("{}{}", " ".repeat(padding), line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn show_welcome_animation() -> Result<(), io::Error> {
    let term = Term::stdout();
    term.clear_screen()?;

    let frames = vec![
        r#"
+================================+
|         S T I T C H            |
|      Stream Manager CLI        |
|                                |
|           Loading...           |
+================================+"#,
        r#"
+================================+
|         S T I T C H            |
|      Stream Manager CLI        |
|               *                |
|           Loading...           |
+================================+"#,
        r#"
+================================+
|         S T I T C H            |
|      Stream Manager CLI        |
|             * * *              |
|           Loading...           |
+================================+"#,
        r#"
+================================+
|         S T I T C H            |
|      Stream Manager CLI        |
|           * * * * *            |
|           Loading...           |
+================================+"#,
    ];

    for frame in &frames {
        term.clear_screen()?;

        // Get terminal dimensions (rows, columns)
        let (term_height, term_width) = term.size();
        
        // Center vertically
        let frame_lines = frame.trim().lines().count();
        let vertical_padding = (term_height as usize).saturating_sub(frame_lines) / 2;

        // Print vertical padding
        for _ in 0..vertical_padding {
            println!();
        }

        // Center horizontally and print
        let centered = center_text(frame.trim(), term_width as usize);
        println!("{}", centered.cyan().bold());
        term.flush()?;
        sleep(Duration::from_millis(200)).await;
    }

    let final_frame = r#"
+================================+
|         S T I T C H            |
|      Stream Manager CLI        |
|           * * * * *            |
|            Ready :)            |
+================================+"#;
    term.clear_screen()?;

    // Get terminal dimensions (rows, columns)
    let (term_height, term_width) = term.size();
    
    // Center vertically
    let frame_lines = final_frame.trim().lines().count();
    let vertical_padding = (term_height as usize).saturating_sub(frame_lines) / 2;

    // Print vertical padding
    for _ in 0..vertical_padding {
        println!();
    }

    // Center horizontally and print
    let centered = center_text(final_frame.trim(), term_width as usize);
    println!("{}", centered.green().bold());

    sleep(Duration::from_millis(500)).await;
    term.clear_screen()?;

    Ok(())
}

pub fn show_spinner(message: &str) -> SpinnerGuard {
    SpinnerGuard::new(message)
}

pub struct SpinnerGuard {
    handle: Option<tokio::task::JoinHandle<()>>,
    stop_signal: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SpinnerGuard {
    fn new(message: &str) -> Self {
        let stop_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop_signal.clone();
        let msg = message.to_string();

        let handle = tokio::spawn(async move {
            let frames = vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;

            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                print!("\r{} {} ", frames[i].cyan(), msg);
                io::stdout().flush().unwrap();
                i = (i + 1) % frames.len();
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
            print!("\r");
            io::stdout().flush().unwrap();
        });

        Self {
            handle: Some(handle),
            stop_signal,
        }
    }

    pub fn success(self, message: &str) {
        self.stop_signal
            .store(true, std::sync::atomic::Ordering::Relaxed);
        println!("\r{}", message);
    }

    pub fn error(self, message: &str) {
        self.stop_signal
            .store(true, std::sync::atomic::Ordering::Relaxed);
        println!("\r{}", message);
    }
}

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        self.stop_signal
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ =
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(handle));
        }
    }
}
