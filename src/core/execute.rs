use std::collections::{HashMap, VecDeque};
use std::iter::FromIterator;
use rand::{thread_rng, Rng};
use core::map::PosHex;
use core::{self, Moves, ObjId, PlayerId, State};
use core::command;
use core::component::{self, Component};
use core::command::Command;
use core::event::{self, ActiveEvent, Event};
use core::effect::{self, Effect};
use core::check::{check, check_attack_at, Error};
use core::movement::Path;

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Phase {
    Pre,
    Post,
}

pub fn execute<F>(state: &mut State, command: &Command, cb: &mut F) -> Result<(), Error>
where
    F: FnMut(&State, &Event, Phase),
{
    debug!("Simulator: do_command: {:?}", command);
    if let Err(err) = check(state, command) {
        error!("Check failed: {:?}", err);
        return Err(err);
    }
    match *command {
        Command::Create(ref command) => execute_create(state, cb, command),
        Command::MoveTo(ref command) => execute_move_to(state, cb, command),
        Command::Attack(ref command) => execute_attack(state, cb, command),
        Command::EndTurn(ref command) => execute_end_turn(state, cb, command),
    }
    Ok(())
}

fn do_event<F>(state: &mut State, cb: &mut F, event: &Event)
where
    F: FnMut(&State, &Event, Phase),
{
    cb(state, event, Phase::Pre);
    event::apply(state, event);
    cb(state, event, Phase::Post);
}

fn execute_move_to<F>(state: &mut State, cb: &mut F, command: &command::MoveTo)
where
    F: FnMut(&State, &Event, Phase),
{
    let id = command.id;
    let mut cost = Some(Moves(1));
    let mut current_path = Vec::new();
    let mut remainder = VecDeque::from_iter(command.path.tiles().iter().cloned());
    while let Some(pos) = remainder.pop_front() {
        if check_reaction_attacks_at(state, id, pos) {
            current_path.push(pos);
            do_move(
                state,
                cb,
                id,
                cost.take(),
                Path::new(current_path.split_off(0)),
            );
            let attack_status = try_execute_reaction_attacks(state, cb, id);
            if attack_status == AttackStatus::Hit {
                return;
            }
        }
        current_path.push(pos);
    }
    do_move(state, cb, command.id, cost.take(), Path::new(current_path));
}

fn do_move<F>(state: &mut State, cb: &mut F, id: ObjId, cost: Option<Moves>, path: Path)
where
    F: FnMut(&State, &Event, Phase),
{
    let cost = cost.unwrap_or(Moves(0));
    let active_event = ActiveEvent::MoveTo(event::MoveTo { id, path, cost });
    let event = Event {
        active_event,
        actor_ids: vec![id],
        effects: HashMap::new(),
    };
    do_event(state, cb, &event);
}

fn check_reaction_attacks_at(state: &mut State, target_id: ObjId, pos: PosHex) -> bool {
    let initial_player_id = state.player_id;
    let mut result = false;
    for obj_id in core::enemy_agent_ids(state, initial_player_id) {
        let command_attack = command::Attack {
            attacker_id: obj_id,
            target_id,
        };
        state.player_id = state.parts.belongs_to.get(obj_id).0;
        if check_attack_at(state, &command_attack, pos).is_ok() {
            result = true;
            break;
        }
    }
    state.player_id = initial_player_id;
    result
}

fn execute_create<F>(state: &mut State, cb: &mut F, command: &command::Create)
where
    F: FnMut(&State, &Event, Phase),
{
    let mut components = state.prototypes.0[&command.prototype].clone();
    if let Some(player_id) = command.owner {
        components.push(Component::BelongsTo(component::BelongsTo(player_id)));
    }
    components.extend_from_slice(&[
        Component::Pos(component::Pos(command.pos)),
        Component::Meta(component::Meta {
            name: command.prototype.clone(),
        }),
    ]);
    let id = state.parts.alloc_id();
    let active_event = ActiveEvent::Create(event::Create {
        pos: command.pos,
        id,
        prototype: command.prototype.clone(),
        components,
    });
    let event = Event {
        active_event,
        actor_ids: vec![id],
        effects: HashMap::new(),
    };
    do_event(state, cb, &event);
}

#[derive(PartialEq, Clone, Debug)]
enum AttackStatus {
    Hit,
    Miss,
}

fn execute_attack_internal<F>(
    state: &mut State,
    cb: &mut F,
    command: &command::Attack,
    mode: event::AttackMode,
) -> AttackStatus
where
    F: FnMut(&State, &Event, Phase),
{
    let active_event = ActiveEvent::Attack(event::Attack {
        attacker_id: command.attacker_id,
        target_id: command.target_id,
        mode,
    });
    let mut effects = HashMap::new();
    let effect = if thread_rng().gen_range(0, 6) < 3 {
        let strength = state.parts.strength.get(command.target_id);
        if strength.strength.0 > 1 {
            Effect::Wound(effect::Wound(core::Strength(1)))
        } else {
            Effect::Kill
        }
    } else {
        Effect::Miss
    };
    let status = match effect {
        Effect::Kill | Effect::Wound(_) => AttackStatus::Hit,
        Effect::Miss => AttackStatus::Miss,
    };
    effects.insert(command.target_id, vec![effect.clone()]);
    let event = Event {
        active_event,
        actor_ids: vec![command.attacker_id],
        effects,
    };
    do_event(state, cb, &event);
    status
}

fn try_execute_reaction_attacks<F>(state: &mut State, cb: &mut F, target_id: ObjId) -> AttackStatus
where
    F: FnMut(&State, &Event, Phase),
{
    let mut status = AttackStatus::Miss;
    let initial_player_id = state.player_id;
    for obj_id in core::enemy_agent_ids(state, initial_player_id) {
        if state.parts.agent.get_opt(obj_id).is_none() {
            // check if target is killed
            continue;
        }
        let command_attack = command::Attack {
            attacker_id: obj_id,
            target_id,
        };
        let command = command::Command::Attack(command_attack.clone());
        state.player_id = state.parts.belongs_to.get(obj_id).0;
        if check(state, &command).is_err() {
            continue;
        }
        let mode = event::AttackMode::Reactive;
        let this_attack_status = execute_attack_internal(state, cb, &command_attack, mode);
        if this_attack_status != AttackStatus::Miss {
            status = this_attack_status;
        }
    }
    state.player_id = initial_player_id;
    status
}

fn execute_attack<F>(state: &mut State, cb: &mut F, command: &command::Attack)
where
    F: FnMut(&State, &Event, Phase),
{
    execute_attack_internal(state, cb, command, event::AttackMode::Active);
    try_execute_reaction_attacks(state, cb, command.attacker_id);
}

fn execute_end_turn<F>(state: &mut State, cb: &mut F, _: &command::EndTurn)
where
    F: FnMut(&State, &Event, Phase),
{
    {
        let player_id_old = state.player_id();
        let active_event = ActiveEvent::EndTurn(event::EndTurn {
            player_id: player_id_old,
        });
        let actor_ids = core::players_agent_ids(state, player_id_old);
        let effects = HashMap::new();
        let event = Event {
            active_event,
            actor_ids,
            effects,
        };
        do_event(state, cb, &event);
    }
    {
        let player_id_new = next_player_id(state);
        let active_event = ActiveEvent::BeginTurn(event::BeginTurn {
            player_id: player_id_new,
        });
        let actor_ids = core::players_agent_ids(state, player_id_new);
        let effects = HashMap::new();
        let event = Event {
            active_event,
            actor_ids,
            effects,
        };
        do_event(state, cb, &event);
    }
}

fn next_player_id(state: &State) -> PlayerId {
    let current_player_id = PlayerId(state.player_id().0 + 1);
    if current_player_id.0 < state.players_count {
        current_player_id
    } else {
        PlayerId(0)
    }
}

fn random_free_pos(state: &State) -> Option<PosHex> {
    let attempts = 30;
    let radius = state.map().radius();
    for _ in 0..attempts {
        let pos = PosHex {
            q: thread_rng().gen_range(-radius.0, radius.0),
            r: thread_rng().gen_range(-radius.0, radius.0),
        };
        if state.map().is_inboard(pos) && !core::is_tile_blocked(state, pos) {
            return Some(pos);
        }
    }
    None
}

fn random_free_sector_pos(state: &State, player_id: PlayerId) -> Option<PosHex> {
    let attempts = 30;
    let radius = state.map().radius();
    let start_sector_width = radius.0;
    for _ in 0..attempts {
        let q = radius.0 - thread_rng().gen_range(0, start_sector_width);
        let pos = PosHex {
            q: match player_id.0 {
                0 => -q,
                1 => q,
                _ => unimplemented!(),
            },
            r: thread_rng().gen_range(-radius.0, radius.0),
        };
        if state.map().is_inboard(pos) && !core::is_tile_blocked(state, pos) {
            return Some(pos);
        }
    }
    None
}

// TODO: improve the API
pub fn create_objects<F>(state: &mut State, cb: &mut F)
where
    F: FnMut(&State, &Event, Phase),
{
    let player_id_initial = state.player_id;
    for &(owner, typename, count) in &[
        (None, "boulder", 10),
        (Some(PlayerId(0)), "swordsman", 2),
        (Some(PlayerId(0)), "spearman", 2),
        (Some(PlayerId(1)), "imp", 9),
    ] {
        if let Some(player_id) = owner {
            state.player_id = player_id;
        }
        for _ in 0..count {
            let pos = match owner {
                Some(player_id) => random_free_sector_pos(state, player_id),
                None => random_free_pos(state),
            }.unwrap();
            let command = Command::Create(command::Create {
                prototype: typename.into(),
                pos,
                owner,
            });
            execute(state, &command, cb).expect("Can't create object");
        }
    }
    state.player_id = player_id_initial;
}
