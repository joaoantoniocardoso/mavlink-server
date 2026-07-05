use std::{future::Future, sync::OnceLock, thread::JoinHandle};

use tokio::{
    runtime::{Handle, Runtime},
    task::JoinHandle as TaskJoinHandle,
};

#[derive(Clone, Debug)]
pub struct PlaneHandles {
    pub data: Handle,
    pub control: Handle,
}

static HANDLES: OnceLock<PlaneHandles> = OnceLock::new();

pub struct PlaneRuntimes {
    control: Runtime,
    _data_thread: JoinHandle<()>,
}

impl PlaneRuntimes {
    pub fn start() -> Self {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);

        let data_thread = std::thread::Builder::new()
            .name("data-plane".into())
            .spawn(move || {
                let data = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("data-plane runtime");

                ready_tx
                    .send(data.handle().clone())
                    .expect("data-plane handle");

                data.block_on(async { std::future::pending::<()>().await });
            })
            .expect("data-plane thread");

        let data_handle = ready_rx.recv().expect("data-plane handle");

        let control = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("control-plane")
            .build()
            .expect("control-plane runtime");

        let handles = PlaneHandles {
            data: data_handle,
            control: control.handle().clone(),
        };

        HANDLES
            .set(handles)
            .expect("plane runtimes already initialized");

        Self {
            control,
            _data_thread: data_thread,
        }
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.control.block_on(future)
    }
}

pub fn handles() -> PlaneHandles {
    ensure_handles()
}

pub fn spawn_data<F, T>(future: F) -> TaskJoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    handles().data.spawn(future)
}

pub fn spawn_control<F, T>(future: F) -> TaskJoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    handles().control.spawn(future)
}

pub fn ensure_handles() -> PlaneHandles {
    if let Some(handles) = HANDLES.get() {
        return handles.clone();
    }

    let handle = Handle::try_current().expect("no tokio runtime and plane runtimes not started");
    let unified = PlaneHandles {
        data: handle.clone(),
        control: handle,
    };
    let _ = HANDLES.set(unified.clone());
    unified
}
