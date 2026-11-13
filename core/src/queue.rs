//! Operaciones sobre la cola de reproducción (puras, para poder probarlas).

use crate::models::QueueItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueOp {
    /// Quita la canción en esa posición.
    Remove(usize),
    /// Sube una posición.
    Up(usize),
    /// «Reproducir a continuación»: la pone la primera.
    ToTop(usize),
    /// Arrastrar: lleva la canción de `from` a la posición `to`.
    Move { from: usize, to: usize },
    /// Al pulsarla: suena ya y salen de la cola las que iban antes que ella.
    JumpTo(usize),
}

/// Aplica la operación y devuelve la cola nueva. Una posición fuera de rango no cambia nada.
pub fn apply(items: &[QueueItem], op: QueueOp) -> Vec<QueueItem> {
    let mut out = items.to_vec();
    match op {
        QueueOp::Remove(i) if i < out.len() => {
            out.remove(i);
        }
        QueueOp::Up(i) if i > 0 && i < out.len() => out.swap(i, i - 1),
        QueueOp::ToTop(i) if i > 0 && i < out.len() => {
            let item = out.remove(i);
            out.insert(0, item);
        }
        QueueOp::Move { from, to } if from < out.len() && to < out.len() && from != to => {
            let item = out.remove(from);
            out.insert(to, item);
        }
        QueueOp::JumpTo(i) if i < out.len() => {
            out.drain(..=i);
        }
        _ => {}
    }
    out
}

/// Identificadores en orden, tal como los pide el motor (`queue_set`).
pub fn uids(items: &[QueueItem]) -> Vec<String> {
    items.iter().map(|i| i.uid.clone()).collect()
}

/// Etiqueta de origen: «EN COLA» (añadida a mano), «AUTOPLAY» (la puso Spotify) o vacío.
pub fn tag(item: &QueueItem) -> &'static str {
    if item.queued {
        "EN COLA"
    } else if item.autoplay == Some(true) {
        "AUTOPLAY"
    } else {
        ""
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;
    use crate::models::Track;

    fn q(ids: &[&str]) -> Vec<QueueItem> {
        ids.iter().map(|id| QueueItem {
            uid: id.to_string(), queued: false, autoplay: None,
            track: Track { id: None, uri: format!("spotify:track:{id}"), name: id.to_string(), artists: vec![], album: None, duration_ms: 1, explicit: None },
        }).collect()
    }

    #[test]
    fn move_down_up_and_jump() {
        let base = q(&["a", "b", "c", "d"]);
        assert_eq!(uids(&apply(&base, QueueOp::Move { from: 0, to: 2 })), ["b", "c", "a", "d"]);
        assert_eq!(uids(&apply(&base, QueueOp::Move { from: 3, to: 1 })), ["a", "d", "b", "c"]);
        assert_eq!(uids(&apply(&base, QueueOp::Move { from: 1, to: 9 })), ["a", "b", "c", "d"]);
        assert_eq!(uids(&apply(&base, QueueOp::JumpTo(1))), ["c", "d"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Track;

    fn item(uid: &str, queued: bool) -> QueueItem {
        QueueItem {
            uid: uid.into(),
            queued,
            autoplay: None,
            track: Track {
                id: None,
                uri: format!("spotify:track:{uid}"),
                name: uid.into(),
                artists: vec![],
                album: None,
                duration_ms: 1000,
                explicit: None,
            },
        }
    }

    fn ids(v: &[QueueItem]) -> Vec<&str> {
        v.iter().map(|i| i.uid.as_str()).collect()
    }

    #[test]
    fn operations() {
        let q = vec![item("a", false), item("b", true), item("c", false)];
        assert_eq!(ids(&apply(&q, QueueOp::Remove(1))), ["a", "c"]);
        assert_eq!(ids(&apply(&q, QueueOp::Up(2))), ["a", "c", "b"]);
        assert_eq!(ids(&apply(&q, QueueOp::ToTop(2))), ["c", "a", "b"]);
    }

    #[test]
    fn out_of_range_and_noops_change_nothing() {
        let q = vec![item("a", false), item("b", false)];
        for op in [QueueOp::Remove(5), QueueOp::Up(0), QueueOp::Up(9), QueueOp::ToTop(0), QueueOp::ToTop(7)] {
            assert_eq!(ids(&apply(&q, op)), ["a", "b"], "{op:?}");
        }
        assert!(apply(&[], QueueOp::Remove(0)).is_empty());
    }

    #[test]
    fn uids_and_tags() {
        let mut q = vec![item("a", true), item("b", false)];
        q[1].autoplay = Some(true);
        assert_eq!(uids(&q), ["a", "b"]);
        assert_eq!((tag(&q[0]), tag(&q[1])), ("EN COLA", "AUTOPLAY"));
        assert_eq!(tag(&item("c", false)), "");
    }
}
