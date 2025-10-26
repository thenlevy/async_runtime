//! https://redixhumayun.github.io/async/2024/10/10/async-runtimes-part-iii.html
use std::{
    cell::RefCell,
    pin::Pin,
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
};

struct Executor {
    tasks: Rc<RefCell<TaskQueue>>,
    next_task_id: usize,
}

impl Executor {
    fn run(&self) {
        loop {
            self.task_queue.borrow_mut().receive();
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

            self.task_queue.borrow_mut().receive();
            if !self.reactor.borrow().waiting_on_events() && self.task_queue.borrow().is_empty() {
                break;
            }

            if self.reactor.borrow().waiting_on_events() {
                match self.wait_for_io() {
                    Ok(events) => self.wake_futures_on_io(events),
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

    fn wait_for_io(&self) -> std::io::Result<Vec<Event>> {
        self.reactor.borrow_mut().poll()
    }

    fn wake_futures_on_io(&self, events: Vec<Event>) {
        let wakers = self.reactor.borrow_mut().get_wakers(events);
        let _ = wakers
            .into_iter()
            .map(|waker| waker.wake())
            .collect::<Vec<_>>();
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
