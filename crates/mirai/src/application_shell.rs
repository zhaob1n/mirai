// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use std::cell::RefCell;
use std::rc::Rc;

use adw::subclass::prelude::*;
use gtk::{gio, glib};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MiraiApplication {
        pub runtime: RefCell<Option<tokio::runtime::Runtime>>,
        pub engines: RefCell<Option<Rc<crate::engines::EnginePool>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MiraiApplication {
        const NAME: &'static str = "MiraiApplication";
        type Type = super::MiraiApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for MiraiApplication {
        fn dispose(&self) {
            self.engines.borrow_mut().take();
            if let Some(runtime) = self.runtime.borrow_mut().take() {
                runtime.shutdown_timeout(std::time::Duration::from_millis(500));
            }
        }
    }
    impl ApplicationImpl for MiraiApplication {}
    impl GtkApplicationImpl for MiraiApplication {}
    impl AdwApplicationImpl for MiraiApplication {}
}

glib::wrapper! {
    pub struct MiraiApplication(ObjectSubclass<imp::MiraiApplication>)
        @extends gio::Application, gtk::Application, adw::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl MiraiApplication {
    pub fn new(application_id: &str, flags: gio::ApplicationFlags) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("mirai-rt")
            .build()
            .expect("could not start the tokio runtime");
        let this: Self = glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", flags)
            .build();
        this.imp().runtime.replace(Some(runtime));
        this.imp()
            .engines
            .replace(Some(Rc::new(crate::engines::EnginePool::default())));
        this
    }

    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.imp()
            .runtime
            .borrow()
            .as_ref()
            .expect("application runtime has shut down")
            .handle()
            .clone()
    }

    pub fn engine_pool(&self) -> Rc<crate::engines::EnginePool> {
        self.imp()
            .engines
            .borrow()
            .as_ref()
            .expect("application engine pool has shut down")
            .clone()
    }

    pub fn shutdown_runtime(&self) {
        self.imp().engines.borrow_mut().take();
        if let Some(runtime) = self.imp().runtime.borrow_mut().take() {
            runtime.shutdown_timeout(std::time::Duration::from_millis(500));
        }
    }
}
