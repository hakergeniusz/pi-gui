mod app;
mod chat;
mod rpc;
mod session_list;
mod style;

use libadwaita::Application;
use libadwaita::prelude::*;

fn main() {
    // Test instances (fake-pi via PI_GUI_PI_BIN) get their own application id
    // so they can never grab the single-instance handoff from the real app.
    let app_id = match std::env::var_os("PI_GUI_PI_BIN") {
        Some(_) => "io.github.hakergeniusz.pi-gui.test",
        None => "io.github.hakergeniusz.pi-gui",
    };
    let app = Application::builder()
        .application_id(app_id)
        .build();

    app.connect_startup(|_app| {
        if let Err(err) = style::load_css() {
            eprintln!("failed to load css: {err}");
        }
    });

    app.connect_activate(|app| {
        let window = app::PiWindow::new(app);
        window.present();
    });

    app.run_with_args(&std::env::args().collect::<Vec<_>>());
}
