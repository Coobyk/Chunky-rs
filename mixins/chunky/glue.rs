// Chunky (Chunky-rs) on the game: the edges between the pure module
// (`game::chunky`) and Horizon — the `/chunky` commands, the task driver
// that hands the sweep's band to the level's streaming, and the JSON state
// in the world's folder. Added to the game as `game::chunky_glue`; the hook
// helpers (register.rs, command.rs, console.rs, level_tick.rs, lifecycle.rs,
// list.rs) call it from the hook points.
//
// The driver mirrors Horizon's own `/pregen` task (game/commands/pregen.rs):
// each level tick it moves the band past the rows whose chunks are lit and
// post-processed, hands the band to the level's streaming, and reports the
// progress now and then.

use crate::game::{
    chunky as m,
    native_hooks::NativeMod,
    native_lifecycle::WorldPhase,
    native_server::{NativeCommand, NativeServerPlayer},
    Game,
};
use horizon_client::{
    streaming::Band,
    text::{self, Text},
};
use std::time::{Duration, Instant};

/// How long the driver pumps the workers each level tick pass, before and
/// after it (the engine's own task pumps between ticks; while the level is
/// in place these slices keep the pipeline full, so the workers idle as
/// little as the caps allow).
const PUMP_MILLIS: u64 = 12;

/// Collect what the workers finished and hand them the next jobs as soon as
/// each one is done, for up to `millis`, while there is work in flight.
fn pump(game: &mut Game, millis: u64) {
    let deadline = Instant::now() + Duration::from_millis(millis);
    while game.streaming.busy() && Instant::now() < deadline {
        game.streaming.wait(Duration::from_millis(2));
        game.update_world();
    }
}

/// `chunky.json` in the world's folder, so the world's backups carry the
/// task. False when there is no world folder or the write failed.
fn save(game: &Game) -> bool {
    game.native_world_dir()
        .map(|dir| m::save(&dir))
        .unwrap_or(false)
}

fn feedback(game: &mut Game, line: impl Into<String>) {
    game.native_feedback(Text::literal(line.into()));
}

/// The progress, to the log and the player who started the task.
fn report(game: &mut Game, owner: Option<u128>, message: Text) {
    log::info!("Chunky: {}", message.plain());
    if let Some(uuid) = owner {
        game.native_tell(uuid, message);
    }
}

fn line_of(task: &m::Task) -> String {
    let paused = if task.paused { "Paused. " } else { "" };
    format!("{paused}Pregenerating {}: {}", task.dim, m::progress_line(task))
}

// ---- Commands -------------------------------------------------------------

pub(in crate::game) fn register(commands: &mut Vec<NativeCommand>) {
    for usage in [
        "chunky start",
        "chunky pause",
        "chunky continue",
        "chunky cancel",
        "chunky status",
        "chunky quiet",
        "chunky spawn",
        "chunky center <x:number> <z:number>",
        "chunky radius <radius:integer>",
        "chunky shape <shape:word>",
    ] {
        // The operator level Chunky asks of its commands.
        commands.push(NativeCommand::op(usage, 2));
    }
    log::info!("Chunky: {} usages pushed", commands.len());
}

pub(in crate::game) fn perform(
    game: &mut Game,
    player: Option<&NativeServerPlayer>,
    command: &str,
    ran: &mut bool,
) {
    if *ran || command.split_whitespace().next() != Some("chunky") {
        return;
    }
    *ran = true;
    let owner = player.map(|p| p.uuid);
    match m::parse(command) {
        m::Action::Help => feedback(game, m::HELP),
        m::Action::Start => start(game, player, owner),
        m::Action::Pause => pause(game, true),
        m::Action::Continue => pause(game, false),
        m::Action::Cancel => cancel(game),
        m::Action::Status => status(game),
        m::Action::Quiet => quiet(game),
        m::Action::Spawn => spawn(game),
        m::Action::Center(x, z) => center(game, x, z),
        m::Action::Radius(radius) => set_radius(game, radius),
        m::Action::Shape(shape) => set_shape(game, &shape),
        m::Action::Unknown(what) => {
            feedback(game, format!("Unknown /chunky command: {what}. {}", m::HELP));
        }
    }
}

fn start(game: &mut Game, player: Option<&NativeServerPlayer>, owner: Option<u128>) {
    if m::TASK.lock().unwrap().is_some() {
        feedback(game, "Chunky is already running: /chunky cancel stops it");
        return;
    }
    let settings = *m::SETTINGS.lock().unwrap();
    // Chunky starts at the selection's centre, else where the command comes
    // from (the player, the console at the world's spawn).
    let (x, z) = match settings.center {
        Some(center) => center,
        None => match player.map(|p| p.position) {
            Some(position) => (position[0].floor() as i32, position[2].floor() as i32),
            None => game
                .spawn
                .map_or((0, 0), |s| (s.x.floor() as i32, s.z.floor() as i32)),
        },
    };
    let radius = settings.radius.clamp(0.0, m::MAX_RADIUS) as i32;
    let sweep = m::Sweep::new((x >> 4, z >> 4), radius >> 4, settings.circle);
    let dim = game.dim.id().to_owned();
    let total = sweep.total;
    let shape = if settings.circle { "circle" } else { "square" };
    *m::TASK.lock().unwrap() = Some(m::Task::new(sweep, dim.clone(), owner));
    feedback(
        game,
        format!(
            "Pregenerating {total} chunks of {dim}: a {shape} of radius {radius} around {x} {z}"
        ),
    );
    save(game);
}

/// Pause or resume the task (`/chunky pause`, `/chunky continue`).
fn pause(game: &mut Game, pausing: bool) {
    let now = m::now_secs();
    let mut guard = m::TASK.lock().unwrap();
    let Some(task) = guard.as_mut() else {
        drop(guard);
        feedback(game, "No pregeneration is running");
        return;
    };
    if pausing {
        task.pause(now);
    } else {
        task.resume(now);
    }
    let line = line_of(task);
    if pausing && game.dim.id() == task.dim {
        game.streaming.set_band(None);
    }
    drop(guard);
    feedback(game, line);
    save(game);
}

fn cancel(game: &mut Game) {
    let Some(task) = m::TASK.lock().unwrap().take() else {
        feedback(game, "No pregeneration is running");
        return;
    };
    if game.dim.id() == task.dim {
        game.streaming.set_band(None);
    }
    feedback(game, format!("Cancelled. {}", m::progress_line(&task)));
    save(game);
}

fn status(game: &mut Game) {
    let settings = *m::SETTINGS.lock().unwrap();
    let shape = if settings.circle { "circle" } else { "square" };
    let center = settings
        .center
        .map_or("the command's source".to_owned(), |(x, z)| format!("{x} {z}"));
    feedback(
        game,
        format!(
            "Chunky: centre {center}, radius {} blocks, shape {shape}, reports {}",
            settings.radius,
            if settings.quiet { "quiet" } else { "on" },
        ),
    );
    let guard = m::TASK.lock().unwrap();
    let Some(task) = guard.as_ref() else {
        drop(guard);
        feedback(game, "No pregeneration is running");
        return;
    };
    let line = line_of(task);
    drop(guard);
    feedback(game, line);
}

fn quiet(game: &mut Game) {
    let quiet = {
        let mut settings = m::SETTINGS.lock().unwrap();
        settings.quiet = !settings.quiet;
        settings.quiet
    };
    feedback(game, format!("Progress reports {}", if quiet { "off" } else { "on" }));
    save(game);
}

fn spawn(game: &mut Game) {
    let Some(s) = game.spawn else {
        feedback(game, "The world's spawn is not known yet");
        return;
    };
    let (x, z) = (s.x.floor() as i32, s.z.floor() as i32);
    m::SETTINGS.lock().unwrap().center = Some((x, z));
    feedback(game, format!("Centre set to {x} {z}"));
    save(game);
}

fn center(game: &mut Game, x: f64, z: f64) {
    let (x, z) = (x.floor() as i32, z.floor() as i32);
    m::SETTINGS.lock().unwrap().center = Some((x, z));
    feedback(game, format!("Centre set to {x} {z}"));
    save(game);
}

fn set_radius(game: &mut Game, radius: f64) {
    if !(0.0..=m::MAX_RADIUS).contains(&radius) {
        feedback(game, format!("Radius must be 0 to {} blocks", m::MAX_RADIUS));
        return;
    }
    m::SETTINGS.lock().unwrap().radius = radius;
    feedback(game, format!("Radius set to {radius} blocks"));
    save(game);
}

fn set_shape(game: &mut Game, shape: &str) {
    match shape {
        "square" => m::SETTINGS.lock().unwrap().circle = false,
        "circle" => m::SETTINGS.lock().unwrap().circle = true,
        other => {
            feedback(
                game,
                format!("Unknown shape {other}: /chunky shape takes square or circle"),
            );
            return;
        }
    }
    feedback(game, format!("Shape set to {shape}"));
    save(game);
}

// ---- The driver -----------------------------------------------------------

/// The spec of a sweep's band, as the level's streaming asks for it.
fn band(spec: m::BandSpec) -> Band {
    Band {
        x0: spec.x0,
        x1: spec.x1,
        z0: spec.z0,
        z1: spec.z1,
        circle: spec.circle,
    }
}

/// Move the task's band past the rows that are lit and hand it to the
/// level's streaming, and report the progress now and then. Called around
/// each level tick pass.
pub(in crate::game) fn level_tick(game: &mut Game, dimension: &str) {
    let now = m::now_secs();
    let mut guard = m::TASK.lock().unwrap();
    let Some(task) = guard.as_mut() else {
        return;
    };
    if task.dim != dimension {
        return;
    }
    if task.paused {
        // A level that fell behind (parked, nobody on it) may hold the band
        // the task set; clear it while paused, whenever it ticks again.
        game.streaming.set_band(None);
        return;
    }
    // A row is done once its chunks are lit and post-processed, as vanilla's
    // are by the time they are FULL: then they may unload and be saved.
    let moved = {
        let streaming = &mut game.streaming;
        let chunks = &game.world.chunks;
        task.sweep.advance(|x, z| {
            streaming.is_lit(x, z)
                && chunks.get(&(x, z)).is_some_and(|c| c.postprocess.is_empty())
        })
    };
    if moved {
        task.moved = Some(now);
    }
    game.streaming.set_band(task.sweep.band().map(band));
    pump(game, PUMP_MILLIS);
    if now - task.moved.unwrap_or(now) >= m::STALL {
        // Nothing advanced for a while: say what the first row waits for,
        // and queue light again for chunks that lost their job.
        task.moved = Some(now);
        let row = task.sweep.first_row();
        let waiting = row
            .iter()
            .filter(|k| {
                game.world
                    .chunks
                    .get(k)
                    .is_some_and(|c| !c.postprocess.is_empty())
            })
            .count();
        let [missing, lighting, unlit] = game.streaming.unstick_light(&mut game.world, &row);
        log::warn!(
            "Chunky: pregeneration waits on row {:?}: {missing} not loaded, {lighting} being lit, \
             {unlit} unlit with no light job (queued again), {waiting} not post-processed; {:?}",
            row.first(),
            game.streaming.stages(),
        );
    }
    let finished = task.sweep.finished();
    let reportable = finished || now - task.reported.unwrap_or(now) >= m::REPORT_EVERY;
    // The line first: the rate window closes at the last report, so it must
    // be read before this one's stamp goes in.
    let line = line_of(task);
    let (owner, reported_done) = (task.owner, task.sweep.done);
    if reportable {
        task.reported = Some(now);
        task.reported_done = reported_done;
    }
    if finished {
        *guard = None;
    }
    drop(guard);
    if reportable {
        let quiet = m::SETTINGS.lock().unwrap().quiet;
        if finished || !quiet {
            let message = if finished {
                Text::literal(format!("Chunky: done. {line}")).with_color(text::GREEN)
            } else {
                Text::literal(line)
            };
            report(game, owner, message);
        }
        // Where the work sits, for the log: a pipeline that fills but leaves
        // the workers idle says so here.
        log::info!("Chunky: stages {:?}", game.streaming.stages());
        save(game);
    }
    if finished {
        game.streaming.set_band(None);
    }
}

/// After the level tick pass: pump again with what the pass's own streaming
/// update scheduled, while the level is still in place.
pub(in crate::game) fn level_tick_post(game: &mut Game, dimension: &str) {
    let guard = m::TASK.lock().unwrap();
    let Some(task) = guard.as_ref() else {
        return;
    };
    if task.dim != dimension || task.paused {
        return;
    }
    drop(guard);
    pump(game, PUMP_MILLIS);
}

// ---- Lifecycle ------------------------------------------------------------

pub(in crate::game) fn lifecycle(game: &mut Game, phase: WorldPhase) {
    match phase {
        WorldPhase::WorldOpenReady => {
            let Some(dir) = game.native_world_dir() else {
                return;
            };
            let Some(mut task) = m::load(&dir) else {
                return;
            };
            // The time the world was closed is not the task's.
            if !task.paused {
                task.resume(m::now_secs());
            }
            let line = m::progress_line(&task);
            let owner = task.owner;
            log::info!("Chunky: continuing the saved task: {line}");
            *m::TASK.lock().unwrap() = Some(task);
            if let Some(uuid) = owner {
                game.native_tell(
                    uuid,
                    Text::literal(format!("Chunky: continuing the saved task: {line}")),
                );
            }
        }
        WorldPhase::WorldCloseFinal | WorldPhase::WorldCloseFailed => {
            let has_task = m::TASK.lock().unwrap().is_some();
            if has_task {
                save(game);
            }
            // The next world opens with Chunky's defaults; its own file
            // restores its own.
            *m::TASK.lock().unwrap() = None;
            *m::SETTINGS.lock().unwrap() = m::Settings::new();
        }
        _ => {}
    }
}

// ---- Options > Mods -------------------------------------------------------

pub(in crate::game) fn list(mods: &mut Vec<NativeMod>) {
    mods.push(NativeMod {
        id: m::ID.to_owned(),
        title: format!("Chunky {}", m::VERSION),
        description: "Chunky's chunk pregenerator, on Horizon's engine. /chunky start".to_owned(),
        keys: Vec::new(),
    });
}
