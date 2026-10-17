//! The pairing window: the Mac's address and port first (as the Mac's
//! pairing window shows them), then the 6-digit code.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use tokio::net::TcpStream;

use crate::app::{self, Command, discovery};
use crate::proto::DEFAULT_PORT;
use crate::proto::pairing::PairingAttempt;

use super::Ui;

struct Pending {
    attempt: PairingAttempt<TcpStream>,
    host: String,
    port: u16,
}

pub fn show(ui: &Rc<Ui>) {
    if let Some(win) = ui.pair_window.borrow().as_ref().and_then(|w| w.upgrade()) {
        win.present();
        return;
    }
    let win = build(ui);
    *ui.pair_window.borrow_mut() = Some(win.downgrade());
    win.present();
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder().label(text).wrap(true).justify(gtk::Justification::Center).build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

fn error_label() -> gtk::Label {
    let l = label("", &["error"]);
    l.set_visible(false);
    l
}

fn show_error(l: &gtk::Label, text: &str) {
    l.set_label(text);
    l.set_visible(true);
}

fn page() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build()
}

fn build(ui: &Rc<Ui>) -> adw::Window {
    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::SlideLeftRight).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&stack));
    let win = adw::Window::builder()
        .title("Pair with a Mac")
        .default_width(460)
        .default_height(600)
        .content(&toolbar)
        .application(&ui.app)
        .build();
    let pending: Rc<RefCell<Option<Pending>>> = Rc::default();

    // --- 1. address -----------------------------------------------------
    let address_page = page();
    address_page.append(&label("Pair with your Mac", &["title-2"]));
    address_page.append(&label(
        "On the Mac, open AV Relay in the menu bar and choose Allow pairing…, then enter the address and port it shows.",
        &["dim-label"],
    ));
    let fields = adw::PreferencesGroup::new();
    let address = adw::EntryRow::builder().title("Mac address").build();
    let port = adw::EntryRow::builder().title("Port").text(DEFAULT_PORT.to_string()).input_purpose(gtk::InputPurpose::Digits).build();
    fields.add(&address);
    fields.add(&port);
    address_page.append(&fields);

    let found = adw::PreferencesGroup::builder().title("On this network").build();
    let looking = adw::ActionRow::builder().title("Looking for Macs…").build();
    found.add(&looking);
    address_page.append(&found);

    let address_error = error_label();
    address_page.append(&address_error);
    let spinner = gtk::Spinner::new();
    let next = gtk::Button::builder().label("Continue").halign(gtk::Align::Center).build();
    next.add_css_class("suggested-action");
    next.add_css_class("pill");
    let row = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
    row.append(&spinner);
    row.append(&next);
    address_page.append(&row);
    stack.add_named(&address_page, Some("address"));

    // --- 2. code --------------------------------------------------------
    let code_page = page();
    code_page.append(&label("Enter the code", &["title-2"]));
    code_page.append(&label(
        "Type the 6-digit code the Mac shows. Each code works once and only while the Mac's pairing window is open.",
        &["dim-label"],
    ));
    let code = gtk::Entry::builder()
        .placeholder_text("000 000")
        .max_length(7)
        .input_purpose(gtk::InputPurpose::Digits)
        .xalign(0.5)
        .halign(gtk::Align::Center)
        .width_chars(9)
        .build();
    code.add_css_class("title-1");
    code_page.append(&code);
    let code_error = error_label();
    code_page.append(&code_error);
    let code_spinner = gtk::Spinner::new();
    let back = gtk::Button::with_label("Back");
    let pair = gtk::Button::with_label("Pair");
    pair.add_css_class("suggested-action");
    pair.add_css_class("pill");
    back.add_css_class("pill");
    let code_row = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
    code_row.append(&back);
    code_row.append(&code_spinner);
    code_row.append(&pair);
    code_page.append(&code_row);
    stack.add_named(&code_page, Some("code"));

    // --- 3. done --------------------------------------------------------
    let done = adw::StatusPage::builder().icon_name("emblem-ok-symbolic").title("Paired").build();
    let close = gtk::Button::with_label("Done");
    close.add_css_class("suggested-action");
    close.add_css_class("pill");
    close.set_halign(gtk::Align::Center);
    done.set_child(Some(&close));
    stack.add_named(&done, Some("done"));

    // Macs answering on Bonjour, to fill the fields with one click.
    {
        let handle = ui.rt.spawn_blocking(|| discovery::browse(Duration::from_secs(3)));
        let (found, looking, address, port) = (found.clone(), looking.clone(), address.clone(), port.clone());
        glib::spawn_future_local(async move {
            let macs = handle.await.unwrap_or_default();
            if macs.is_empty() {
                looking.set_title("None found. Type the address the Mac shows.");
                return;
            }
            found.remove(&looking);
            for mac in macs {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&mac.name))
                    .subtitle(format!("{}:{}", mac.address, mac.port))
                    .activatable(true)
                    .build();
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let (address, port) = (address.clone(), port.clone());
                row.connect_activated(move |_| {
                    address.set_text(&mac.address.to_string());
                    port.set_text(&mac.port.to_string());
                });
                found.add(&row);
            }
        });
    }

    let start = {
        let (ui, stack, pending, address, port, err, spinner, next, code) =
            (ui.clone(), stack.clone(), pending.clone(), address.clone(), port.clone(), address_error.clone(), spinner.clone(), next.clone(), code.clone());
        move || {
            let host = address.text().trim().to_string();
            let Ok(port_nr) = port.text().trim().parse::<u16>() else {
                show_error(&err, "The port is a number, normally 48100.");
                return;
            };
            if host.is_empty() {
                show_error(&err, "Enter the Mac's address, as its pairing window shows it.");
                return;
            }
            err.set_visible(false);
            next.set_sensitive(false);
            spinner.start();
            let handle = ui.rt.spawn({
                let host = host.clone();
                async move { app::start_pairing(&host, port_nr).await }
            });
            let (stack, pending, err, spinner, next, code) = (stack.clone(), pending.clone(), err.clone(), spinner.clone(), next.clone(), code.clone());
            glib::spawn_future_local(async move {
                let result = handle.await;
                spinner.stop();
                next.set_sensitive(true);
                match result {
                    Ok(Ok(attempt)) => {
                        *pending.borrow_mut() = Some(Pending { attempt, host, port: port_nr });
                        code.set_text("");
                        stack.set_visible_child_name("code");
                        code.grab_focus();
                    }
                    Ok(Err(e)) => show_error(&err, &format!("{e:#}")),
                    Err(e) => show_error(&err, &format!("{e}")),
                }
            });
        }
    };
    let start = Rc::new(start);
    next.connect_clicked({
        let start = start.clone();
        move |_| start()
    });
    address.connect_entry_activated({
        let start = start.clone();
        move |_| start()
    });
    port.connect_entry_activated(move |_| start());

    let finish = {
        let (ui, stack, pending, code, err, address_err, spinner, pair, done) =
            (ui.clone(), stack.clone(), pending.clone(), code.clone(), code_error.clone(), address_error.clone(), code_spinner.clone(), pair.clone(), done.clone());
        move || {
            let digits: String = code.text().chars().filter(|c| !c.is_whitespace()).collect();
            if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_digit()) {
                show_error(&err, "The code has 6 digits.");
                return;
            }
            let Some(Pending { attempt, host, port }) = pending.borrow_mut().take() else { return };
            err.set_visible(false);
            pair.set_sensitive(false);
            spinner.start();
            let core = ui.core.clone();
            let handle = ui.rt.spawn(async move { app::finish_pairing(&core, attempt, &host, port, &digits).await });
            let (ui, stack, err, address_err, spinner, pair, done) =
                (ui.clone(), stack.clone(), err.clone(), address_err.clone(), spinner.clone(), pair.clone(), done.clone());
            glib::spawn_future_local(async move {
                let result = handle.await;
                spinner.stop();
                pair.set_sensitive(true);
                match result {
                    Ok(Ok(name)) => {
                        done.set_title(&format!("Paired with {name}"));
                        done.set_description(Some(&format!(
                            "“MacBook Camera” and “MacBook Microphone” are here whenever {} is reachable. Pick them in your apps.",
                            glib::markup_escape_text(&name)
                        )));
                        stack.set_visible_child_name("done");
                        let _ = ui.commands.send(Command::Paired);
                    }
                    // The code is used up either way: start again from the Mac.
                    Ok(Err(e)) => {
                        stack.set_visible_child_name("address");
                        show_error(&address_err, &format!("{e:#}"));
                    }
                    Err(e) => {
                        stack.set_visible_child_name("address");
                        show_error(&address_err, &format!("{e}"));
                    }
                }
                let _ = &err;
            });
        }
    };
    let finish = Rc::new(finish);
    pair.connect_clicked({
        let finish = finish.clone();
        move |_| finish()
    });
    code.connect_activate(move |_| finish());

    back.connect_clicked({
        let (stack, pending) = (stack.clone(), pending.clone());
        move |_| {
            pending.borrow_mut().take();
            stack.set_visible_child_name("address");
        }
    });
    close.connect_clicked({
        let win = win.downgrade();
        move |_| {
            if let Some(w) = win.upgrade() {
                w.close();
            }
        }
    });
    win.connect_close_request(move |_| {
        // Closing mid-pairing hangs up. Before the code was sent, the Mac's
        // code stays valid; after, it's used up either way.
        pending.borrow_mut().take();
        glib::Propagation::Proceed
    });
    win
}
