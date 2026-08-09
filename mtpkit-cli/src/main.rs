//! mtpkit — MTP client CLI (browse, put, get, mkdir, rm — over USB, no
//! kernel driver or FUSE needed).

mod complete;
mod shell;

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Result, bail};
use rustyline::config::{CompletionType, Config};
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;

use mtpkit::session::Session;
use mtpkit::usb;

use complete::MtpHelper;
use shell::{Shell, USAGE};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }

    let devices = usb::list_devices()?;
    let Some(device) = devices.first() else {
        bail!(
            "no MTP device found — connect the device and unlock its screen \
             (run with MTPKIT_DEBUG=1 to see the USB interfaces considered)"
        );
    };
    if devices.len() > 1 {
        eprintln!("multiple MTP devices found, using the first one");
    }
    eprintln!("connected: {}", device.name);

    let transport = usb::Transport::open(device)?;
    let session = Session::open(transport)?;
    let shell = Rc::new(RefCell::new(Shell::new(session)?));

    if !args.is_empty() {
        for command in &args {
            if !shell.borrow_mut().exec_line(command)? {
                break;
            }
        }
        return Ok(());
    }

    let config = Config::builder()
        .completion_type(CompletionType::List)
        .build();
    let mut rl: rustyline::Editor<MtpHelper, DefaultHistory> =
        rustyline::Editor::with_config(config)?;
    rl.set_helper(Some(MtpHelper::new(Rc::clone(&shell))));
    loop {
        let prompt = format!("{}> ", shell.borrow().pwd());
        match rl.readline(&prompt) {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line.as_str());
                let result = shell.borrow_mut().exec_line(&line);
                match result {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => eprintln!("error: {e:#}"),
                }
            }
            Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted) => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
