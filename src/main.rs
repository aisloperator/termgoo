//! TermGoo: a GTK4 terminal emulator.
//! Created by AI Sloperator with Claude Code. No license is given.

mod cli;
mod config;
mod desktop;
mod input;
mod pty;
mod term;
mod view;
mod window;

use cli::{Options, Parsed};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::process::ExitCode;
use std::rc::Rc;

fn main() -> ExitCode {
    let options: Options = match cli::parse(std::env::args().skip(1)) {
        Ok(Parsed::Run(o)) => o,
        Ok(Parsed::Help) => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            println!("TermGoo {}", env!("CARGO_PKG_VERSION"));
            println!("Created by AI Sloperator with Claude Code");
            println!("No license is given.");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("termgoo: {e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };

    glib::set_prgname(Some("termgoo"));
    glib::set_application_name("TermGoo");
    // Each process is its own terminal, so no application ID or D-Bus instance is wanted.
    let app = gtk::Application::builder().flags(gio::ApplicationFlags::NON_UNIQUE).build();
    let failed = Rc::new(Cell::new(false));
    app.connect_activate({
        let failed = failed.clone();
        move |app| {
            if let Err(e) = window::open(app, &options) {
                eprintln!("termgoo: {e}");
                failed.set(true);
                app.quit();
            }
        }
    });
    // Our options are not GTK's to look at.
    app.run_with_args(&["termgoo"]);
    if failed.get() { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
