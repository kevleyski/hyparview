use rand::{Rng, ThreadRng};
use std::collections::VecDeque;

use {Action, Message, NodeOptions, TimeToLive};

#[derive(Debug)]
pub struct Node<T, R = ThreadRng> {
    id: T,
    actions: VecDeque<Action<T>>,
    active_view: Vec<T>,
    passive_view: Vec<T>,
    options: NodeOptions<R>,
}
impl<T> Node<T, ThreadRng>
where
    T: Clone + Eq,
{
    pub fn new(node_id: T) -> Self {
        Node::with_options(node_id, NodeOptions::default())
    }
}
impl<T, R> Node<T, R>
where
    T: Clone + Eq,
    R: Rng,
{
    pub fn with_options(node_id: T, options: NodeOptions<R>) -> Self {
        Node {
            id: node_id,
            actions: VecDeque::new(),
            active_view: Vec::with_capacity(options.max_active_view_size as usize),
            passive_view: Vec::with_capacity(options.max_passive_view_size as usize),
            options,
        }
    }

    pub fn join(&mut self, contact_node_id: T) {
        send(&mut self.actions, contact_node_id, Message::join(&self.id));
    }

    pub fn disconnect(&mut self, node: T) {
        self.remove_from_active_view(&node);
        if !self.is_active_view_full() {
            if let Some(node) = self.select_random_from_passive_view() {
                let high_priority = self.active_view.is_empty();
                let message = Message::neighbor(&self.id, high_priority);
                send(&mut self.actions, node, message);
            }
        }
    }

    pub fn handle_message(&mut self, message: Message<T>) {
        let sender = message.sender().clone();
        match message {
            Message::Join { sender } => self.handle_join(sender),
            Message::ForwardJoin {
                sender,
                new_node,
                ttl,
            } => self.handle_forward_join(sender, new_node, ttl),
            Message::Neighbor {
                sender,
                high_priority,
            } => self.handle_neighbor(sender, high_priority),
            Message::Shuffle {
                sender,
                origin,
                nodes,
                ttl,
            } => self.handle_shuffle(sender, origin, nodes, ttl),
            Message::ShuffleReply { sender, nodes } => self.handle_shuffle_reply(sender, nodes),
        }
        self.disconnect_unless_active_view_node(sender);
    }

    pub fn shuffle_passive_view(&mut self) {
        if let Some(node) = self.select_random_from_active_view() {
            self.options.rng.shuffle(&mut self.active_view);
            self.options.rng.shuffle(&mut self.passive_view);

            let av_size = self.options.shuffle_active_view_size as usize;
            let pv_size = self.options.shuffle_passive_view_size as usize;
            let shuffle_size = 1 + av_size + pv_size;

            let mut nodes = Vec::with_capacity(shuffle_size);
            nodes.push(self.id.clone());
            nodes.extend(self.active_view.iter().take(av_size).cloned());
            nodes.extend(self.passive_view.iter().take(pv_size).cloned());

            let ttl = TimeToLive::new(self.options.active_random_walk_len);
            let message = Message::shuffle(&self.id, self.id.clone(), nodes, ttl);
            send(&mut self.actions, node, message);
        }
    }

    pub fn fill_active_view(&mut self) {
        if self.is_active_view_full() {
            return;
        }

        let shortage = (self.options.max_active_view_size as usize) - self.active_view.len();
        self.options.rng.shuffle(&mut self.passive_view);
        for n in self.passive_view.iter().take(shortage) {
            let high_priority = self.active_view.is_empty();
            let message = Message::neighbor(&self.id, high_priority);
            send(&mut self.actions, n.clone(), message);
        }
    }

    pub fn poll_action(&mut self) -> Option<Action<T>> {
        self.actions.pop_front()
    }

    pub fn id(&self) -> &T {
        &self.id
    }

    pub fn active_view(&self) -> &[T] {
        &self.active_view
    }

    pub fn passive_view(&self) -> &[T] {
        &self.passive_view
    }

    pub fn options(&self) -> &NodeOptions<R> {
        &self.options
    }

    pub fn options_mut(&mut self) -> &mut NodeOptions<R> {
        &mut self.options
    }

    fn is_active_view_full(&self) -> bool {
        self.active_view.len() >= self.options.max_active_view_size as usize
    }

    fn is_passive_view_full(&self) -> bool {
        self.passive_view.len() >= self.options.max_passive_view_size as usize
    }

    fn handle_join(&mut self, new_node: T) {
        self.add_to_active_view(new_node.clone());
        let ttl = TimeToLive::new(self.options.active_random_walk_len);
        for n in self.active_view.iter().filter(|n| **n != new_node) {
            let message = Message::forward_join(&self.id, new_node.clone(), ttl);
            send(&mut self.actions, n.clone(), message);
        }
    }

    fn handle_forward_join(&mut self, sender: T, new_node: T, ttl: TimeToLive) {
        if ttl.is_expired() || self.active_view.is_empty() {
            self.add_to_active_view(new_node);
        } else {
            if ttl.as_u8() == self.options.passive_random_walk_len {
                self.add_to_passive_view(new_node.clone());
            }
            if let Some(next) = self.select_forwarding_destination(&[&sender]) {
                let message = Message::forward_join(&self.id, new_node, ttl.decrement());
                send(&mut self.actions, next, message);
            }
        }
    }

    fn handle_neighbor(&mut self, sender: T, high_priority: bool) {
        if high_priority || !self.is_active_view_full() {
            self.add_to_active_view(sender.clone());
        }
    }

    fn handle_shuffle(&mut self, sender: T, origin: T, nodes: Vec<T>, ttl: TimeToLive) {
        if ttl.is_expired() {
            self.options.rng.shuffle(&mut self.passive_view);
            let reply_nodes = self.passive_view
                .iter()
                .take(nodes.len())
                .cloned()
                .collect();
            let message = Message::shuffle_reply(&self.id, reply_nodes);
            send(&mut self.actions, origin.clone(), message);
            self.add_shuffled_nodes_to_passive_view(nodes);
        } else {
            if let Some(destination) = self.select_forwarding_destination(&[&origin, &sender]) {
                let message = Message::shuffle(&self.id, origin, nodes, ttl.decrement());
                send(&mut self.actions, destination, message);
            }
        }
    }

    fn handle_shuffle_reply(&mut self, _sender: T, nodes: Vec<T>) {
        self.add_shuffled_nodes_to_passive_view(nodes);
    }

    fn add_shuffled_nodes_to_passive_view(&mut self, nodes: Vec<T>) {
        for n in nodes {
            self.add_to_passive_view(n);
        }
    }

    fn add_to_active_view(&mut self, node: T) {
        if self.active_view.contains(&node) {
            return;
        }
        self.remove_random_from_active_view_if_full();
        self.remove_from_passive_view(&node);
        self.active_view.push(node.clone());
        self.actions.push_back(Action::notify_up(node.clone()));
        send(&mut self.actions, node, Message::neighbor(&self.id, true));
    }

    fn add_to_passive_view(&mut self, node: T) {
        if self.active_view.contains(&node) || self.passive_view.contains(&node) {
            return;
        }
        self.remove_random_from_passive_view_if_full();
        self.passive_view.push(node);
    }

    fn remove_from_active_view(&mut self, node: &T) {
        let index = self.active_view.iter().position(|n| n == node);
        if let Some(i) = index {
            self.remove_from_active_view_by_index(i);
        }
    }

    fn remove_from_active_view_by_index(&mut self, i: usize) {
        let node = self.active_view.swap_remove(i);
        self.actions.push_back(Action::notify_down(node.clone()));
        self.actions.push_back(Action::disconnect(node.clone()));
        self.add_to_passive_view(node);
    }

    fn remove_random_from_active_view_if_full(&mut self) {
        if self.is_active_view_full() {
            let i = self.options.rng.gen_range(0, self.active_view.len());
            self.remove_from_active_view_by_index(i);
        }
    }

    fn remove_from_passive_view(&mut self, node: &T) {
        let position = self.passive_view.iter().position(|n| n == node);
        if let Some(i) = position {
            self.passive_view.swap_remove(i);
        }
    }

    fn remove_random_from_passive_view_if_full(&mut self) {
        if self.is_passive_view_full() {
            let i = self.options.rng.gen_range(0, self.passive_view.len());
            self.passive_view.swap_remove(i);
        }
    }

    fn disconnect_unless_active_view_node(&mut self, node: T) {
        if !self.active_view.contains(&node) {
            self.actions.push_back(Action::disconnect(node));
        }
    }

    fn select_forwarding_destination(&mut self, excludes: &[&T]) -> Option<T> {
        let mut i = 0;
        let mut tail = self.active_view.len();
        while i < tail && tail != 0 {
            let is_not_candidate = excludes.contains(&&self.active_view[i]);
            if is_not_candidate {
                self.active_view.swap(i, tail - 1);
                tail -= 1;
            } else {
                i += 1;
            }
        }

        if tail == 0 {
            None
        } else {
            let i = self.options.rng.gen_range(0, tail);
            Some(self.active_view[i].clone())
        }
    }

    fn select_random_from_active_view(&mut self) -> Option<T> {
        if self.active_view.is_empty() {
            None
        } else {
            let i = self.options.rng.gen_range(0, self.active_view.len());
            Some(self.active_view[i].clone())
        }
    }

    fn select_random_from_passive_view(&mut self) -> Option<T> {
        if self.passive_view.is_empty() {
            None
        } else {
            let i = self.options.rng.gen_range(0, self.passive_view.len());
            Some(self.passive_view[i].clone())
        }
    }
}

fn send<T>(actions: &mut VecDeque<Action<T>>, destination: T, message: Message<T>) {
    let action = Action::Send {
        destination,
        message,
    };
    actions.push_back(action);
}
