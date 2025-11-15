//! https://redixhumayun.github.io/async/2024/10/10/async-runtimes-part-iii.html

use crate::a_la_mano::reactor::Reactor;

use {
    core::task::{Context, RawWaker, RawWakerVTable, Waker},
    std::{
        cell::{Cell, RefCell},
        pin::Pin,
        rc::Rc,
        sync::{
            atomic::{AtomicPtr, Ordering},
            mpsc::{self, Receiver, Sender},
        },
    },
};

pub struct Executor {
    task_queue: Rc<RefCell<TaskQueue>>,
    next_task_id: Cell<usize>,
}

static HANDLE: AtomicPtr<Executor> = AtomicPtr::new(core::ptr::null_mut());

impl Executor {
    fn get() -> &'static Self {
        let p = HANDLE.load(Ordering::Relaxed);
        if p.is_null() {
            let p = Box::new(Executor::new());
            HANDLE.store(Box::leak(p) as *mut Executor, Ordering::Relaxed);
            Executor::get()
        } else {
            unsafe { p.as_ref().unwrap_unchecked() }
        }
    }

    fn run(&self) {
        loop {
            self.task_queue.borrow_mut().receive();

            // Run all tasks that are ready to make progress.
            println!("Running {} tasks", self.task_queue.borrow().len());
            loop {
                let task = {
                    if let Some(task) = self.task_queue.borrow_mut().pop() {
                        task
                    } else {
                        break;
                    }
                };

                let waker = MyWaker::new(Rc::clone(&task), self.task_queue.borrow().sender());
                let mut context = Context::from_waker(&waker);
                match task.future.borrow_mut().as_mut().poll(&mut context) {
                    std::task::Poll::Ready(_output) => {}
                    std::task::Poll::Pending => {}
                };
            }

            eprintln!("Recieving tasks");
            self.task_queue.borrow_mut().receive();
            eprintln!(
                "After running tasks, {} tasks remain",
                self.task_queue.borrow().len()
            );
            if !Reactor::waiting_on_events() && self.task_queue.borrow().is_empty() {
                break;
            }

            if Reactor::waiting_on_events() {
                match Reactor::react() {
                    Ok(()) => {}
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::Interrupted {
                            break;
                        }
                        eprintln!("Error while waiting for IO events :{}", e);
                    }
                }
            }
        }
    }

    pub fn spawn<F>(future: F) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()> + 'static,
    {
        let this = Self::get();
        let task = Rc::new(Task {
            id: this.next_task_id.get(),
            future: RefCell::new(Box::pin(future)),
        });
        this.next_task_id.update(|x| x + 1);
        this.task_queue.borrow().sender().send(task).unwrap();
        Reactor::notify()
    }

    pub fn block_on<F>(future: F)
    where
        F: std::future::Future<Output = ()> + 'static,
    {
        let this = Self::get();
        let task = Rc::new(Task {
            id: this.next_task_id.get(),
            future: RefCell::new(Box::pin(future)),
        });
        this.next_task_id.update(|x| x + 1);
        this.task_queue.borrow().sender().send(task).unwrap();
        this.run();
    }

    pub fn new() -> Self {
        Self {
            task_queue: Rc::new(RefCell::new(TaskQueue::new())),
            next_task_id: Cell::new(0),
        }
    }
}

pub struct TaskQueue {
    pub tasks: Vec<Rc<Task>>,
    sender: Sender<Rc<Task>>,
    receiver: Receiver<Rc<Task>>,
}

impl TaskQueue {
    pub fn new() -> Self {
        let (sender, recv) = mpsc::channel();
        Self {
            tasks: Vec::new(),
            sender,
            receiver: recv,
        }
    }

    pub fn sender(&self) -> Sender<Rc<Task>> {
        self.sender.clone()
    }

    pub fn receive(&mut self) {
        while let Ok(task) = self.receiver.try_recv() {
            self.tasks.push(task);
        }
    }

    pub fn pop(&mut self) -> Option<Rc<Task>> {
        self.tasks.pop()
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.len() == 0
    }
}

pub struct Task {
    pub id: usize,
    pub future: RefCell<Pin<Box<dyn Future<Output = ()> + 'static>>>,
}

pub struct MyWaker {
    /// The task whose executions is to be resumed when `wake` or `wake_by_ref` is called.
    task: Rc<Task>,
    /// By sending the task through this sender, we enqueue it back into the executor's task queue.
    sender: Sender<Rc<Task>>,
}

impl MyWaker {
    const VTABLE: RawWakerVTable =
        RawWakerVTable::new(Self::clone, Self::wake, Self::wake_by_ref, Self::drop);

    pub fn new(task: Rc<Task>, sender: Sender<Rc<Task>>) -> Waker {
        let pointer = Rc::into_raw(Rc::new(MyWaker { task, sender })) as *const ();
        let vtable = &MyWaker::VTABLE;

        // SAFETY: In the context of this project, it's okay to create a Waker with a non-thread
        // safe interface because we won't be spawning threads.
        unsafe { Waker::new(pointer, vtable) }
    }

    unsafe fn clone(ptr: *const ()) -> RawWaker {
        let waker = std::mem::ManuallyDrop::new(unsafe {
            // SAFETY:
            // * `ptr` was created either from MyWaker::new or MyWaker::clone. In both cases,
            // it was previously returned by a call to Rc<MyWaker>::into_raw.
            // * The reference count is
            //  - Set to 1 on creation in MyWaker::new (by Rc::<MyWaker>::new).
            //  - Incremented by 1 in MyWaker::clone below
            //   - Decremented by 1 when either the Waker is dropped and calls MyWaker::drop or when
            //     the Waker calls MyWaker::wake which transfers ownership to the wake function.
            Rc::from_raw(ptr as *const MyWaker)
        });
        let cloned_waker = Rc::clone(&waker);
        let raw_pointer = Rc::into_raw(cloned_waker);
        RawWaker::new(raw_pointer as *const (), &Self::VTABLE)
    }

    unsafe fn wake(ptr: *const ()) {
        eprintln!("Waker is waking a task");
        let waker = unsafe {
            // SAFETY:
            // * `ptr` was created either from MyWaker::new or MyWaker::clone. In both cases,
            // it was previously returned by a call to Rc<MyWaker>::into_raw.
            // * See the implementation of MyWaker::clone for details on why the value is dropped
            //   only once.
            Rc::from_raw(ptr as *const MyWaker)
        };
        waker.sender.send(Rc::clone(&waker.task)).unwrap();
        // `waker` is dropped here, decrementing the reference count by 1.
        // This is intended as `wake` takes ownership of the waker.
    }

    unsafe fn wake_by_ref(ptr: *const ()) {
        let waker = unsafe {
            // SAFETY:
            // * `ptr` was created either from MyWaker::new or MyWaker::clone. In both cases,
            // it was previously returned by a call to Rc<MyWaker>::into_raw.
            // * See the implementation of MyWaker::clone for details on why the value is dropped
            //   only once.
            &*(ptr as *const MyWaker)
        };
        waker.sender.send(Rc::clone(&waker.task)).unwrap();
        // In this context, the ressources are borrowed, so we don't decrement the reference count.
    }

    unsafe fn drop(ptr: *const ()) {
        drop(unsafe {
            // SAFETY:
            // * `ptr` was created either from MyWaker::new or MyWaker::clone. In both cases,
            // it was previously returned by a call to Rc<MyWaker>::into_raw.
            // * See the implementation of MyWaker::clone for details on why the value is dropped
            //   only once.
            Rc::from_raw(ptr as *const MyWaker)
        });
    }
}
