//! Event Loop Executor
//! Either spawns a new event loop, or re-uses provided one.

use std::{io, thread};
use std::sync::mpsc;
use tokio;
use num_cpus;

use core::futures::{self, sync::oneshot, Future};



/// Possibly uninitialized event loop executor.
#[derive(Debug)]
pub enum UninitializedExecutor {
	/// Shared instance of executor.
	Shared(tokio::runtime::TaskExecutor),
	/// Event Loop should be spawned by the transport.
	Unspawned,
}

impl UninitializedExecutor {
	/// Initializes executor.
	/// In case there is no shared executor, will spawn a new event loop.
	/// Dropping `Executor` closes the loop.
	pub fn initialize(self) -> io::Result<Executor> {
		self.init_with_name("event.loop")
	}

	/// Initializes executor.
	/// In case there is no shared executor, will spawn a new event loop.
	/// Dropping `Executor` closes the loop.
	pub fn init_with_name<T: Into<String>>(self, name: T) -> io::Result<Executor> {
		match self {
			UninitializedExecutor::Shared(executor) => Ok(Executor::Shared(executor)),
			UninitializedExecutor::Unspawned => RpcEventLoop::with_name(Some(name.into())).map(Executor::Spawned),
		}
	}
}

/// Initialized Executor
#[derive(Debug)]
pub enum Executor {
	/// Shared instance
	Shared(tokio::runtime::TaskExecutor),
	/// Spawned Event Loop
	Spawned(RpcEventLoop),
}

impl Executor {
	/// Get tokio executor associated with this event loop.
	pub fn executor(&self) -> tokio::runtime::TaskExecutor {
		match *self {
			Executor::Shared(ref executor) => executor.clone(),
			Executor::Spawned(ref eloop) => eloop.executor(),
		}
	}

	/// Spawn a future onto the Tokio runtime.
	pub fn spawn<F>(&self, future: F)
	where
		F: Future<Item = (), Error = ()> + Send + 'static,
	{
		self.executor().spawn(future)
	}

	/// Closes underlying event loop (if any!).
	// pub fn close(self) {
	// 	if let Executor::Spawned(eloop) = self {
	// 		eloop.close()
	// 	}
	// }

	/// Wait for underlying event loop to finish (if any!).
	pub fn wait(self) {
		if let Executor::Spawned(eloop) = self {
			let _ = eloop.wait();
		}
	}

	pub fn take_close(&mut self) -> Option<oneshot::Sender<()>> {
		match *self {
			Executor::Shared(_) => None,
			Executor::Spawned(ref mut eloop) => eloop.take_close(),
		}
	}
}

/// A handle to running event loop. Dropping the handle will cause event loop to finish.
#[derive(Debug)]
pub struct RpcEventLoop {
	executor: tokio::runtime::TaskExecutor,
	close: Option<oneshot::Sender<()>>,
	handle: Option<thread::JoinHandle<()>>,
}


impl RpcEventLoop {
	/// Spawns a new thread with the `EventLoop`.
	pub fn spawn() -> io::Result<Self> {
		RpcEventLoop::with_name(None)
	}

	pub fn take_close(&mut self) -> Option<oneshot::Sender<()>> {
		self.close.take()
	}

	/// Spawns a new named thread with the `EventLoop`.
	pub fn with_name(name: Option<String>) -> io::Result<Self> {
		let (stop, stopped) = futures::oneshot();
		let (tx, rx) = mpsc::channel();
		let mut tb = thread::Builder::new();
		if let Some(name) = name {
			tb = tb.name(name);
		}

		let handle = tb.spawn(move || {
			let pool_size = match num_cpus::get_physical() {
				1 => 1,
				2...4 => 2,
				_ => 3,
			};

			let runtime = tokio::runtime::Builder::new()
				.core_threads(pool_size)
				.name_prefix("jsonrpc-eventloop-")
				.build();

			match runtime {
				Ok(mut runtime) => {
					tx.send(Ok(runtime.executor())).expect("Rx is blocking upper thread.");
					let terminate = stopped
						.map(|_| ())
						.map_err(|_| ());
					runtime.block_on(terminate);
				},
				Err(err) => {
					tx.send(Err(err)).expect("Rx is blocking upper thread.");
				}
			}
		}).expect("Couldn't spawn a thread.");

		let exec = rx.recv().expect("tx is transfered to a newly spawned thread.");

		exec.map(|executor| RpcEventLoop {
			executor,
			close: Some(stop),
			handle: Some(handle),
		})
	}

	/// Get executor for this event loop.
	pub fn executor(&self) -> tokio::runtime::TaskExecutor {
		self.executor.clone()
	}

	/// Blocks current thread and waits until the event loop is finished.
	pub fn wait(mut self) -> thread::Result<()> {
		self.handle.take().expect("Handle is always set before self is consumed.").join()
	}
}
