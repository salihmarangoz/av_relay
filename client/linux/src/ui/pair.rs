//! The pairing window: which sender first, picked from those found on the
//! network or typed in (address and port, as the sender's pairing window
//! shows them), then the 6-digit code.

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

/// A sender found on the network, and the check mark its row shows while
/// it's selected.
struct Found {
    host: String,
    port: u16,
    check: gtk::Image,
}

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
        .title("Pair with a sender")
        .default_width(460)
        .default_height(620)
        .content(&toolbar)
        .application(&ui.app)
        .build();
    let pending: Rc<RefCell<Option<Pending>>> = Rc::default();

    // --- 1. which sender: found on the network, or typed in ---------------
    let address_page = page();
    address_page.append(&label("Pair with a sender", &["title-2"]));
    address_page.append(&label(
        "On the sender, the computer whose camera and microphone you want to use, choose Allow Pairing… in AV Relay's menu. It shows a code.",
        &["dim-label"],
    ));
    let views = adw::ViewStack::new();
    let switcher = adw::ViewSwitcher::builder().stack(&views).policy(adw::ViewSwitcherPolicy::Wide).halign(gtk::Align::Center).build();
    address_page.append(&switcher);

    // Found: senders answering on Bonjour. Clicking one selects it.
    let found_page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    let found_group = adw::PreferencesGroup::builder().title("Senders on this network").build();
    let refresh = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text("Search again").valign(gtk::Align::Center).build();
    refresh.add_css_class("flat");
    found_group.set_header_suffix(Some(&refresh));
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).activate_on_single_click(false).build();
    list.add_css_class("boxed-list");
    found_group.add(&list);
    found_page.append(&found_group);
    views.add_titled_with_icon(&found_page, Some("found"), "Auto-discovery", "network-wireless-symbolic");

    // Manual: the address and port the sender's pairing window shows.
    let manual_page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    manual_page.append(&label("Type the address and port that the sender's pairing window shows.", &["dim-label"]));
    let fields = adw::PreferencesGroup::new();
    let address = adw::EntryRow::builder().title("Address").build();
    let port = adw::EntryRow::builder().title("Port").text(DEFAULT_PORT.to_string()).input_purpose(gtk::InputPurpose::Digits).build();
    fields.add(&address);
    fields.add(&port);
    manual_page.append(&fields);
    views.add_titled_with_icon(&manual_page, Some("manual"), "Manual", "document-edit-symbolic");
    address_page.append(&views);

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
        "Type the 6-digit code the sender shows. Each code works once and only while the sender's pairing window is open.",
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

    // The senders found, in the list's order.
    let senders: Rc<RefCell<Vec<Found>>> = Rc::default();
    // Continue needs a selected sender on the first tab; the second checks
    // its fields when clicked.
    let update_next = {
        let (views, list, next) = (views.clone(), list.clone(), next.clone());
        move || next.set_sensitive(views.visible_child_name().as_deref() == Some("manual") || list.selected_row().is_some())
    };
    let update_next = Rc::new(update_next);
    let search = {
        let (ui, list, senders, refresh, views, update_next) =
            (ui.clone(), list.clone(), senders.clone(), refresh.clone(), views.clone(), update_next.clone());
        move || {
            list.remove_all();
            senders.borrow_mut().clear();
            let looking = adw::ActionRow::builder().title("Looking for senders…").activatable(false).selectable(false).build();
            looking.add_prefix(&gtk::Spinner::builder().spinning(true).build());
            list.append(&looking);
            refresh.set_sensitive(false);
            update_next();
            let handle = ui.rt.spawn_blocking(|| discovery::browse(Duration::from_secs(3)));
            let (list, senders, refresh, views, update_next) =
                (list.clone(), senders.clone(), refresh.clone(), views.clone(), update_next.clone());
            glib::spawn_future_local(async move {
                let found = handle.await.unwrap_or_default();
                list.remove_all();
                refresh.set_sensitive(true);
                if found.is_empty() {
                    let none = adw::ActionRow::builder()
                        .title("No sender found")
                        .subtitle("Check that AV Relay runs and is switched on there, on the same network, or type its address.")
                        .activatable(false)
                        .selectable(false)
                        .build();
                    let manual = gtk::Button::builder().label("Type it in").valign(gtk::Align::Center).build();
                    manual.connect_clicked(move |_| views.set_visible_child_name("manual"));
                    none.add_suffix(&manual);
                    list.append(&none);
                    return;
                }
                for sender in found {
                    let row = adw::ActionRow::builder()
                        .title(glib::markup_escape_text(&sender.name))
                        .subtitle(format!("{}:{}", sender.address, sender.port))
                        .build();
                    row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
                    let check = gtk::Image::from_icon_name("object-select-symbolic");
                    check.set_visible(false);
                    row.add_suffix(&check);
                    list.append(&row);
                    senders.borrow_mut().push(Found { host: sender.address.to_string(), port: sender.port, check });
                }
                // One sender: nothing to choose.
                if senders.borrow().len() == 1 {
                    list.select_row(list.row_at_index(0).as_ref());
                }
                update_next();
            });
        }
    };
    let search = Rc::new(search);
    search();
    refresh.connect_clicked({
        let search = search.clone();
        move |_| search()
    });
    // The selected sender shows a check mark, so the choice is plain to see.
    list.connect_selected_rows_changed({
        let (senders, update_next) = (senders.clone(), update_next.clone());
        move |list| {
            for (i, found) in senders.borrow().iter().enumerate() {
                found.check.set_visible(list.row_at_index(i as i32).is_some_and(|r| r.is_selected()));
            }
            update_next();
        }
    });
    views.connect_visible_child_name_notify({
        let update_next = update_next.clone();
        move |_| update_next()
    });

    // Enter or a double-click can come again while connecting: one at a time.
    let connecting = Rc::new(std::cell::Cell::new(false));
    let start = {
        let (ui, stack, pending, address, port, err, spinner, next, code) =
            (ui.clone(), stack.clone(), pending.clone(), address.clone(), port.clone(), address_error.clone(), spinner.clone(), next.clone(), code.clone());
        let (views, list, senders, connecting) = (views.clone(), list.clone(), senders.clone(), connecting.clone());
        move || {
            if connecting.get() {
                return;
            }
            let (host, port_nr) = if views.visible_child_name().as_deref() == Some("manual") {
                let host = address.text().trim().to_string();
                let Ok(port_nr) = port.text().trim().parse::<u16>() else {
                    show_error(&err, "The port is a number, normally 48100.");
                    return;
                };
                if host.is_empty() {
                    show_error(&err, "Enter the sender's address, as its pairing window shows it.");
                    return;
                }
                (host, port_nr)
            } else {
                let picked = list.selected_row().and_then(|r| senders.borrow().get(r.index() as usize).map(|f| (f.host.clone(), f.port)));
                let Some(picked) = picked else {
                    show_error(&err, "Pick a sender, or type its address under Manual.");
                    return;
                };
                picked
            };
            err.set_visible(false);
            next.set_sensitive(false);
            connecting.set(true);
            spinner.start();
            let handle = ui.rt.spawn({
                let host = host.clone();
                async move { app::start_pairing(&host, port_nr).await }
            });
            let (stack, pending, err, spinner, next, code, connecting) =
                (stack.clone(), pending.clone(), err.clone(), spinner.clone(), next.clone(), code.clone(), connecting.clone());
            glib::spawn_future_local(async move {
                let result = handle.await;
                spinner.stop();
                connecting.set(false);
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
    // Double-click or Enter on a sender goes on right away.
    list.connect_row_activated({
        let start = start.clone();
        move |list, row| {
            list.select_row(Some(row));
            start()
        }
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
                            "“AV Relay Camera” and “AV Relay Microphone” are here whenever {} is reachable. Pick them in your apps.",
                            glib::markup_escape_text(&name)
                        )));
                        stack.set_visible_child_name("done");
                        let _ = ui.commands.send(Command::Paired);
                    }
                    // The code is used up either way: start again from the sender.
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
        // Closing mid-pairing hangs up. Before the code was sent, the sender's
        // code stays valid; after, it's used up either way.
        pending.borrow_mut().take();
        glib::Propagation::Proceed
    });
    win
}
