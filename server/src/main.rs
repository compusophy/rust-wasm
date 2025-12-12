use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Duration};
use futures_util::{StreamExt, SinkExt};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use std::env;
use tokio::sync::broadcast;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PlayerInfo {
    id: i32,
    chunk_x: i32,
    chunk_y: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct UnitState {
    x: f32,
    y: f32,
    hp: f32,
    kind: u8,
    carry_wood: f32,
    carry_stone: f32,
    carry_gold: f32,
    carry_food: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct UnitDTO {
    owner_id: i32,
    unit_idx: usize,
    x: f32,
    y: f32,
    kind: u8,
    hp: f32,
    #[serde(default)]
    carry_wood: f32,
    #[serde(default)]
    carry_stone: f32,
    #[serde(default)]
    carry_gold: f32,
    #[serde(default)]
    carry_food: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct BuildingDTO {
    id: i32,
    owner_id: i32,
    kind: u8,
    tile_x: i32,
    tile_y: i32,
    hp: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
enum GameMessage {
    Join { version: u32, token: Option<String> },
    Welcome { player_id: i32, chunk_x: i32, chunk_y: i32, players: Vec<PlayerInfo>, units: Vec<UnitDTO>, buildings: Vec<BuildingDTO>, token: String, resources: Resources, pop_cap: i32, pop_used: i32 },
    NewPlayer { player: PlayerInfo },
    UnitMove { player_id: i32, unit_idx: usize, x: f32, y: f32 },
    UnitSync { player_id: i32, unit_idx: usize, x: f32, y: f32 },
    SpawnUnit,
    TrainUnit { building_id: i32, kind: u8 },
    UnitSpawned { unit: UnitDTO },
    Build { kind: u8, tile_x: i32, tile_y: i32 },
    BuildProgress { tile_x: i32, tile_y: i32, kind: u8, progress: f32 },
    BuildingSpawned { building: BuildingDTO },
    AssignGather { unit_ids: Vec<usize>, target_x: i32, target_y: i32, kind: u8 },
    TowerShot { x1: f32, y1: f32, x2: f32, y2: f32 },
    UnitDied { owner_id: i32, unit_idx: usize },
    BuildingDestroyed { tile_x: i32, tile_y: i32 },
    UnitHp { owner_id: i32, unit_idx: usize, hp: f32 },
    BuildingHp { tile_x: i32, tile_y: i32, hp: f32 },
    ResourceUpdate { player_id: i32, resources: Resources, pop_cap: i32, pop_used: i32 },
    DeleteUnit { unit_idx: usize },
    DeleteBuilding { tile_x: i32, tile_y: i32 },
    UnitCarry { owner_id: i32, unit_idx: usize, carry_wood: f32, carry_stone: f32, carry_gold: f32, carry_food: f32 },
    Error { message: String },
}

// Default fallback, but DB overrides this
const MIN_CLIENT_VERSION_DEFAULT: u32 = 22;

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
struct Resources {
    wood: f32,
    stone: f32,
    gold: f32,
    food: f32,
}

impl Resources {
    fn new(wood: f32, stone: f32, gold: f32, food: f32) -> Self {
        Resources { wood, stone, gold, food }
    }
    fn has(&self, cost: &Resources) -> bool {
        self.wood >= cost.wood && self.stone >= cost.stone && self.gold >= cost.gold && self.food >= cost.food
    }
    fn spend(&mut self, cost: &Resources) -> bool {
        if self.has(cost) {
            self.wood -= cost.wood;
            self.stone -= cost.stone;
            self.gold -= cost.gold;
            self.food -= cost.food;
            true
        } else {
            false
        }
    }
}

const COST_WALL: Resources = Resources { wood: 1.0, stone: 5.0, gold: 0.0, food: 0.0 };
const COST_FARM: Resources = Resources { wood: 30.0, stone: 0.0, gold: 0.0, food: 0.0 };
const COST_HOUSE: Resources = Resources { wood: 25.0, stone: 0.0, gold: 0.0, food: 0.0 };
const COST_TOWER: Resources = Resources { wood: 0.0, stone: 40.0, gold: 0.0, food: 0.0 };
const COST_BARRACKS: Resources = Resources { wood: 60.0, stone: 0.0, gold: 0.0, food: 0.0 };
const COST_LUMBER_MILL: Resources = Resources { wood: 30.0, stone: 10.0, gold: 0.0, food: 0.0 };
const COST_MINING_CAMP: Resources = Resources { wood: 30.0, stone: 10.0, gold: 0.0, food: 0.0 };
const COST_WHEAT_MILL: Resources = Resources { wood: 30.0, stone: 10.0, gold: 0.0, food: 0.0 };
const COST_WORKER: Resources = Resources { wood: 0.0, stone: 0.0, gold: 0.0, food: 50.0 };
const COST_WARRIOR: Resources = Resources { wood: 0.0, stone: 0.0, gold: 20.0, food: 40.0 };

const FARM_FOOD_PER_SEC: f32 = 10.0 / 60.0;
const TREE_WOOD_PER_SEC: f32 = 8.0 / 60.0;
const STONE_PER_SEC: f32 = 6.0 / 60.0;
const GOLD_PER_SEC: f32 = 6.0 / 60.0;
const WORKER_HP: f32 = 50.0;
const WARRIOR_HP: f32 = 120.0;
const TOWN_HP: f32 = 800.0;
const WALL_HP: f32 = 200.0;
const TOWER_HP: f32 = 300.0;
const FARM_HP: f32 = 220.0;
const HOUSE_HP: f32 = 220.0;
const BARRACKS_HP: f32 = 260.0;
const LUMBER_HP: f32 = 220.0;
const MINING_HP: f32 = 220.0;
const WHEAT_HP: f32 = 220.0;
const TOWER_DAMAGE: f32 = 25.0;
const WARRIOR_RANGE: f32 = 48.0;
const WARRIOR_DPS: f32 = 30.0;
const POP_FROM_HOUSE: i32 = 1;
const TILE_SIZE: f32 = 16.0;
const CARRY_CAP: f32 = 20.0;
const WOOD_NODE_AMOUNT: f32 = 120.0;
const STONE_NODE_AMOUNT: f32 = 120.0;
const GOLD_NODE_AMOUNT: f32 = 120.0;
const FOOD_NODE_AMOUNT: f32 = 100.0;
// Per-tick gather amounts (tuned faster but not instant)
const GATHER_WOOD_TICK: f32 = 2.0;
const GATHER_STONE_TICK: f32 = 2.0;
const GATHER_GOLD_TICK: f32 = 2.0;
const GATHER_FOOD_TICK: f32 = 2.0;

fn cost_for_kind(kind: u8) -> Resources {
    match kind {
        1 => COST_WALL,
        2 => COST_FARM,
        3 => COST_HOUSE,
        4 => COST_TOWER,
        5 => COST_BARRACKS,
        6 => COST_LUMBER_MILL,
        7 => COST_MINING_CAMP,
        8 => COST_WHEAT_MILL,
        _ => Resources::new(0.0, 0.0, 0.0, 0.0),
    }
}

fn hp_for_kind(kind: u8) -> f32 {
    match kind {
        0 => TOWN_HP,
        1 => WALL_HP,
        2 => FARM_HP,
        3 => HOUSE_HP,
        4 => TOWER_HP,
        5 => BARRACKS_HP,
        6 => LUMBER_HP,
        7 => MINING_HP,
        8 => WHEAT_HP,
        _ => 200.0,
    }
}

#[derive(Clone, Copy)]
struct ResourceNode {
    kind: u8,
    remaining: f32,
}

#[derive(Clone, Copy)]
struct BuildTask {
    owner_id: i32,
    kind: u8,
    tile_x: i32,
    tile_y: i32,
    progress: f32,
}

#[derive(Clone, Copy)]
struct TrainTask {
    owner_id: i32,
    kind: u8,
    progress: f32,
    chunk_x: i32,
    chunk_y: i32,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
struct GatherTask {
    kind: u8,
    target_x: i32,
    target_y: i32,
}

struct GlobalState {
    next_id: i32,
    players: HashMap<i32, PlayerInfo>,
    units: HashMap<i32, Vec<UnitState>>,
    // Memory mode persistence (Token -> PlayerID)
    tokens: HashMap<String, i32>, 
    resources: HashMap<i32, Resources>,
    pop_cap: HashMap<i32, i32>,
    building_progress: HashMap<(i32, i32), BuildTask>, // (tile_x, tile_y) -> task
    training_tasks: Vec<TrainTask>,
    gather_tasks: HashMap<(i32, usize), GatherTask>, // (owner_id, unit_idx)
    buildings: Vec<BuildingDTO>,
    resource_nodes: HashMap<(i32, i32), ResourceNode>, // (tile_x, tile_y)
}

impl GlobalState {
    fn new() -> Self {
        GlobalState {
            next_id: 1,
            players: HashMap::new(),
            units: HashMap::new(),
            tokens: HashMap::new(),
            resources: HashMap::new(),
            pop_cap: HashMap::new(),
            building_progress: HashMap::new(),
            training_tasks: Vec::new(),
            gather_tasks: HashMap::new(),
            buildings: Vec::new(),
            resource_nodes: HashMap::new(),
        }
    }

    fn assign_next_position(n: i32) -> (i32, i32) {
        if n == 0 { return (0, 0); }
        
        // Tweak: Reduce distance between players
        // Previously: 1 chunk per player.
        // New: Pack 4 players into 1 chunk (2x2 grid within chunk?) 
        // OR simpler: Just spiral but fill every chunk (which we do).
        
        // If the user feels map is "WAY TOO BIG", maybe our Chunks (32x32) are too huge?
        // 32 tiles * 16px = 512px. That's barely a screen width.
        // The issue might be that they spawn 1 chunk apart.
        
        // Let's keep the spiral but maybe we don't skip chunks?
        // The current algorithm spirals 0,0 -> 1,0 -> 1,1 -> 0,1 ...
        // This IS filling every chunk.
        
        // Maybe the issue is just visual "void".
        // Let's try to put players closer by using "sub-chunk" addressing?
        // No, let's just stick to 1 player per chunk for now but acknowledge
        // that 512px is not "far".
        
        // Wait, the spiral logic might be spreading them out too much if I implemented it wrong.
        // Let's check the spiral logic.
        // It looks standard Ulam spiral.
        
        let mut x = 0;
        let mut y = 0;
        let mut d = 1;
        let mut m = 1;
        let mut count = 0;
        loop {
            for _ in 0..m {
                x += d;
                count += 1;
                if count == n { return (x, y); }
            }
            for _ in 0..m {
                y += d;
                count += 1;
                if count == n { return (x, y); }
            }
            d = -d;
            m += 1;
        }
    }

    fn spawn_units(cx: i32, cy: i32) -> Vec<UnitState> {
        let chunk_size = 32.0;
        let tile_size = 16.0;
        
        // Town Center is at tile (cx * chunk_size + mid, cy * chunk_size + mid)
        // where mid = chunk_size / 2 = 16
        // Its TOP-LEFT in world coords is that tile * tile_size
        let mid = chunk_size / 2.0;
        let tc_tile_x = cx as f32 * chunk_size + mid;
        let tc_tile_y = cy as f32 * chunk_size + mid;
        
        // Town Center occupies 1 tile (16x16 px) starting from its TOP-LEFT
        // Spawn units below and to the right of the Town Center
        let tc_world_x = tc_tile_x * tile_size;
        let tc_world_y = tc_tile_y * tile_size;
        
        // Unit positions: offset from Town Center's top-left
        // Place them 2 tiles below the TC, spread horizontally
        vec![
            UnitState { x: tc_world_x + tile_size * 0.5, y: tc_world_y + tile_size * 2.0, hp: WORKER_HP, kind: 0, carry_wood: 0.0, carry_stone: 0.0, carry_gold: 0.0, carry_food: 0.0 },
            UnitState { x: tc_world_x + tile_size * 1.5, y: tc_world_y + tile_size * 2.0, hp: WORKER_HP, kind: 0, carry_wood: 0.0, carry_stone: 0.0, carry_gold: 0.0, carry_food: 0.0 },
        ]
    }

    fn find_building(&self, owner: i32, id: i32) -> Option<BuildingDTO> {
        self.buildings.iter().find(|b| b.owner_id == owner && b.id == id).cloned()
    }

    fn is_tile_blocked(&self, tx: i32, ty: i32) -> bool {
        // Block if building already present
        if self.buildings.iter().any(|b| b.tile_x == tx && b.tile_y == ty) {
            return true;
        }
        // Block if a unit is standing on tile
        for units in self.units.values() {
            for u in units {
                let utx = (u.x / TILE_SIZE).floor() as i32;
                let uty = (u.y / TILE_SIZE).floor() as i32;
                if utx == tx && uty == ty {
                    return true;
                }
            }
        }
        false
    }
}

fn dropoff_near(gs: &GlobalState, owner: i32, ux: f32, uy: f32, res_kind: u8) -> bool {
    let allowed: &[u8] = match res_kind {
        2 => &[0, 6],        // TC or Lumber Mill
        3 | 4 => &[0, 7],    // TC or Mining Camp
        5 => &[0, 8],        // TC or Wheat Mill
        _ => &[0],
    };
    let mut ok = false;
    let radius = TILE_SIZE * 1.2; // ~1 tile reach to avoid instant deposits
    let r2 = radius * radius;
    for b in &gs.buildings {
        if b.owner_id != owner { continue; }
        if !allowed.contains(&b.kind) { continue; }
        let bx = b.tile_x as f32 * TILE_SIZE + TILE_SIZE / 2.0;
        let by = b.tile_y as f32 * TILE_SIZE + TILE_SIZE / 2.0;
        let dx = ux - bx;
        let dy = uy - by;
        if (dx*dx + dy*dy) <= r2 {
            ok = true;
            break;
        }
    }
    ok
}

const MIN_START_RES: Resources = Resources { wood: 200.0, stone: 160.0, gold: 60.0, food: 300.0 };

fn default_resources() -> Resources {
    MIN_START_RES
}

fn default_pop_cap() -> i32 {
    5
}

#[tokio::main]
async fn main() {
    std::panic::set_hook(Box::new(|info| {
        println!("CRITICAL PANIC: {:?}", info);
    }));

    let port = env::var("PORT").unwrap_or_else(|_| "9001".to_string());
    let addr = format!("0.0.0.0:{}", port);
    let (tx, _rx) = broadcast::channel(100);
    let state = Arc::new(Mutex::new(GlobalState::new()));

    // Build progress tick loop (server authoritative)
    {
        let tx_clone = tx.clone();
        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
            let mut tick_count: u64 = 0;
            loop {
                interval.tick().await;
                tick_count += 1;
                if tick_count % 150 == 0 {
                    println!("Game Loop Alive. Tick: {}. DB: disabled", tick_count);
                } else if tick_count % 10 == 0 {
                    // Low-frequency heartbeat to confirm it's not stuck
                    // println!("[TRACE] Tick {}", tick_count); 
                }

                let mut to_spawn: Vec<BuildTask> = Vec::new();
                let mut to_spawn_units: Vec<TrainTask> = Vec::new();
                let mut resource_updates: Vec<(i32, Resources, i32, i32)> = Vec::new();
                let mut shots: Vec<(f32, f32, f32, f32, i32)> = Vec::new(); // shot with owner
                let mut unit_hp_updates: Vec<(i32, usize, f32)> = Vec::new();
                let mut unit_deaths: Vec<(i32, usize)> = Vec::new();
                let mut pop_updates: Vec<i32> = Vec::new(); // owners needing pop recount
                let mut building_hp_updates: Vec<(i32, i32, f32)> = Vec::new();
                let mut building_deaths: Vec<(i32, i32, i32)> = Vec::new(); // tile_x, tile_y, owner
                let mut canceled_builds: Vec<BuildTask> = Vec::new();

                // Snapshot phase
                let gather_tasks: Vec<(i32, usize, GatherTask)>;
                let units_snapshot: Vec<(i32, usize, f32, f32, u8)>;
                let buildings_snapshot: Vec<(usize, i32, i32, i32, f32, u8)>;
                let towers_snapshot: Vec<(i32, f32, f32)>;
                {
                    // Use try_lock to detect contention
                    if let Ok(mut gs) = state_clone.try_lock() {
                        // println!("[TRACE] Tick {} Locked", tick_count);
                        let mut finished = Vec::new();
                        // Snapshot worker positions to avoid borrowing conflicts while mutating build progress.
                        let mut worker_positions: HashMap<i32, Vec<(f32, f32)>> = HashMap::new();
                        for (owner, units) in gs.units.iter() {
                            for u in units {
                                if u.kind == 0 {
                                    worker_positions.entry(*owner).or_default().push((u.x, u.y));
                                }
                            }
                        }

                        for (key, task) in gs.building_progress.iter_mut() {
                            // Require at least one friendly worker near the build site to advance progress.
                            let bx = task.tile_x as f32 * TILE_SIZE + TILE_SIZE / 2.0;
                            let by = task.tile_y as f32 * TILE_SIZE + TILE_SIZE / 2.0;
                            let mut worker_count = 0usize;
                            if let Some(units) = worker_positions.get(&task.owner_id) {
                                for (ux, uy) in units {
                                    let dx = *ux - bx;
                                    let dy = *uy - by;
                                    // Allow ~1.5 tiles radius for building
                                    if (dx*dx + dy*dy) <= (TILE_SIZE * 1.5).powi(2) {
                                        worker_count += 1;
                                    }
                                }
                            }
                            if worker_count == 0 {
                                // No worker in range; cancel wall build and refund
                                if task.kind == 1 {
                                    // Notify clients to clear progress
                                    let _ = tx_clone.send(serde_json::to_string(&GameMessage::BuildProgress {
                                        tile_x: task.tile_x,
                                        tile_y: task.tile_y,
                                        kind: task.kind,
                                        progress: -1.0,
                                    }).unwrap_or_default());
                                    canceled_builds.push(*task);
                                    finished.push(*key); // remove from progress map
                                }
                                continue;
                            }

                            // Scale progress with nearby workers (diminishing cap at 4 workers)
                            let effective_workers = worker_count.min(4) as f32;
                            task.progress += 0.12 * effective_workers; // base ~1.6s per worker, faster with helpers
                        // Ignored result to avoid panic on empty receivers
                        let _ = tx_clone.send(serde_json::to_string(&GameMessage::BuildProgress {
                            tile_x: task.tile_x,
                            tile_y: task.tile_y,
                            kind: task.kind,
                            progress: task.progress.min(1.0),
                        }).unwrap_or_default());
                        if task.progress >= 1.0 {
                                finished.push(*key);
                                to_spawn.push(*task);
                            }
                        }
                        for k in finished {
                            gs.building_progress.remove(&k);
                        }
                        
                        // Unit Training Progress
                        let mut finished_train = Vec::new();
                        for (idx, task) in gs.training_tasks.iter_mut().enumerate() {
                            task.progress += 0.05; // 4s training
                            if task.progress >= 1.0 {
                                finished_train.push(idx);
                                to_spawn_units.push(*task);
                            }
                        }
                        for idx in finished_train.iter().rev() {
                            gs.training_tasks.remove(*idx);
                        }
                        
                        gather_tasks = gs.gather_tasks.iter().map(|((owner, uid), g)| (*owner, *uid, *g)).collect();
                        units_snapshot = gs.units.iter()
                            .flat_map(|(owner, us)| us.iter().enumerate().map(move |(i, u)| (*owner, i, u.x, u.y, u.kind)))
                            .collect();
                        buildings_snapshot = gs.buildings.iter().enumerate()
                            .map(|(i, b)| (i, b.owner_id, b.tile_x, b.tile_y, b.hp, b.kind))
                            .collect();
                        towers_snapshot = gs.buildings.iter()
                            .filter(|b| b.kind == 4)
                            .map(|b| (b.owner_id, b.tile_x as f32 * 16.0 + 8.0, b.tile_y as f32 * 16.0 + 8.0))
                            .collect();
                        // println!("[TRACE] Tick {} Snapshot Done", tick_count);
                    } else {
                        // println!("[WARN] MainLoop skipped tick - Lock contention");
                        continue; // Skip this tick if locked
                    }
                }

                // println!("[TRACE] Tick {} Processing", tick_count);

                // Gathering tick (carry + deposit)
                {
                    let mut gs = state_clone.lock().await;
                    for (owner, uid, gtask) in gather_tasks {
                        // Ensure resource node exists and get remaining (scope 1)
                        let key = (gtask.target_x, gtask.target_y);
                        let mut node_remaining = {
                            let entry = gs.resource_nodes.entry(key).or_insert(ResourceNode {
                                kind: gtask.kind,
                                remaining: match gtask.kind {
                                    2 => WOOD_NODE_AMOUNT,
                                    3 => STONE_NODE_AMOUNT,
                                    4 => GOLD_NODE_AMOUNT,
                                    5 => FOOD_NODE_AMOUNT,
                                    _ => WOOD_NODE_AMOUNT,
                                },
                            });
                            entry.remaining
                        };

                        // Snapshot unit state (immutable) to avoid overlapping borrows
                        let (ux, uy, gathered_kind, mut c_wood, mut c_stone, mut c_gold, mut c_food) = {
                            let Some(units) = gs.units.get(&owner) else { continue };
                            if uid >= units.len() { continue; }
                            let u = &units[uid];
                            if u.kind != 0 { continue; } // only workers gather
                            (u.x, u.y, gtask.kind, u.carry_wood, u.carry_stone, u.carry_gold, u.carry_food)
                        };

                        // Early deposit if at dropoff (allows manual turn-in even if not full)
                        let total_carry = c_wood + c_stone + c_gold + c_food;
                        let at_drop = dropoff_near(&gs, owner, ux, uy, gtask.kind);
                        if total_carry > 0.0 && at_drop {
                            let (dw, ds, dg, df) = {
                                if let Some(units) = gs.units.get_mut(&owner) {
                                    if uid < units.len() {
                                        let u = &mut units[uid];
                                        let dw = u.carry_wood;
                                        let ds = u.carry_stone;
                                        let dg = u.carry_gold;
                                        let df = u.carry_food;
                                        u.carry_wood = 0.0;
                                        u.carry_stone = 0.0;
                                        u.carry_gold = 0.0;
                                        u.carry_food = 0.0;
                                        (dw, ds, dg, df)
                                    } else { (0.0, 0.0, 0.0, 0.0) }
                                } else { (0.0, 0.0, 0.0, 0.0) }
                            };

                            // If nothing drained, skip
                            if (dw + ds + dg + df) == 0.0 {
                                continue;
                            }

                            let (res_snapshot, pop_cap, pop_used) = {
                                let entry = gs.resources.entry(owner).or_insert(default_resources());
                                entry.wood += dw;
                                entry.stone += ds;
                                entry.gold += dg;
                                entry.food += df;
                                let res_snapshot = *entry;
                                let pop_cap = *gs.pop_cap.get(&owner).unwrap_or(&default_pop_cap());
                                let pop_used = gs.units.get(&owner).map(|u2| u2.len() as i32).unwrap_or(0);
                                (res_snapshot, pop_cap, pop_used)
                            };

                            // broadcast carry update
                            let _ = tx_clone.send(serde_json::to_string(&GameMessage::UnitCarry {
                                owner_id: owner,
                                unit_idx: uid,
                                carry_wood: 0.0,
                                carry_stone: 0.0,
                                carry_gold: 0.0,
                                carry_food: 0.0,
                            }).unwrap_or_default());
                            // broadcast resources snapshot
                            resource_updates.push((owner, res_snapshot, pop_cap, pop_used));
                            continue;
                        }

                        // Must be near the target to gather (lenient radius)
                        let tx = gtask.target_x as f32 * TILE_SIZE + TILE_SIZE / 2.0;
                        let ty = gtask.target_y as f32 * TILE_SIZE + TILE_SIZE / 2.0;
                        let dx = ux - tx;
                        let dy = uy - ty;
                        if (dx * dx + dy * dy) > (TILE_SIZE * 2.5).powi(2) {
                            continue;
                        }

                        // Re-borrow unit mutably for gathering updates
                        let Some(units) = gs.units.get_mut(&owner) else { continue };
                        if uid >= units.len() { continue; }
                        let u = &mut units[uid];

                        // Choose carry target and rate (fast per-tick gather)
                            {
                                let (target, rate) = match gtask.kind {
                                2 => (&mut c_wood, GATHER_WOOD_TICK),
                                3 => (&mut c_stone, GATHER_STONE_TICK),
                                4 => (&mut c_gold, GATHER_GOLD_TICK),
                                5 => (&mut c_food, GATHER_FOOD_TICK),
                                _ => (&mut c_wood, GATHER_WOOD_TICK),
                                };
                                let room = (CARRY_CAP - *target).max(0.0);
                                if room > 0.0 && node_remaining > 0.0 {
                                    let amt = rate.min(room).min(node_remaining);
                                    *target += amt;
                                    node_remaining -= amt;
                                }
                            }

                            // Write back carries to unit
                            u.carry_wood = c_wood;
                            u.carry_stone = c_stone;
                            u.carry_gold = c_gold;
                            u.carry_food = c_food;

                        // Deposit (scope 3) after unit borrow released
                        let total_carry = c_wood + c_stone + c_gold + c_food;
                        let target_filled = match gathered_kind {
                            2 => c_wood >= CARRY_CAP,
                            3 => c_stone >= CARRY_CAP,
                            4 => c_gold >= CARRY_CAP,
                            5 => c_food >= CARRY_CAP,
                            _ => c_wood >= CARRY_CAP,
                        };
                        let mut carry_changed = false;
                        let at_dropoff = dropoff_near(&gs, owner, ux, uy, gathered_kind);
                        // Deposit only when full (or node empty) AND near dropoff
                        let should_deposit = total_carry > 0.0 && (target_filled || node_remaining <= 0.0) && at_dropoff;
                        if should_deposit {
                            let entry = gs.resources.entry(owner).or_insert(default_resources());
                            entry.wood += c_wood;
                            entry.stone += c_stone;
                            entry.gold += c_gold;
                            entry.food += c_food;
                            c_wood = 0.0;
                            c_stone = 0.0;
                            c_gold = 0.0;
                            c_food = 0.0;
                            carry_changed = true;
                        } else if total_carry > 0.0 {
                            carry_changed = true;
                        }

                        // Update node (scope 4)
                        if node_remaining <= 0.0 {
                            gs.resource_nodes.remove(&key);
                            gs.gather_tasks.remove(&(owner, uid));
                        } else if let Some(entry) = gs.resource_nodes.get_mut(&key) {
                            entry.remaining = node_remaining;
                        }

                        if carry_changed {
                            let _ = tx_clone.send(serde_json::to_string(&GameMessage::UnitCarry {
                                owner_id: owner,
                                unit_idx: uid,
                                carry_wood: c_wood,
                                carry_stone: c_stone,
                                carry_gold: c_gold,
                                carry_food: c_food,
                            }).unwrap_or_default());
                        }

                        let res_snapshot = *gs.resources.get(&owner).unwrap_or(&default_resources());
                        let pop_cap = *gs.pop_cap.get(&owner).unwrap_or(&default_pop_cap());
                        let pop_used = gs.units.get(&owner).map(|u2| u2.len() as i32).unwrap_or(0);
                        resource_updates.push((owner, res_snapshot, pop_cap, pop_used));
                    }
                }

                // Warrior targeting using snapshots
                let mut unit_damage: Vec<(i32, usize, f32)> = Vec::new();
                let mut building_damage: Vec<(usize, f32)> = Vec::new();
                for (owner, _idx, ux, uy, kind) in &units_snapshot {
                    if *kind != 1 { continue; }
                    let mut best_unit: Option<(i32, usize, f32)> = None;
                    for (opid, oidx, ox, oy, _ok) in &units_snapshot {
                        if *opid == *owner { continue; }
                        let dx = ox - ux;
                        let dy = oy - uy;
                        let dist = (dx*dx + dy*dy).sqrt();
                        if dist < WARRIOR_RANGE && (best_unit.is_none() || dist < best_unit.unwrap().2) {
                            best_unit = Some((*opid, *oidx, dist));
                        }
                    }
                    if let Some((opid, oidx, _)) = best_unit {
                        unit_damage.push((opid, oidx, WARRIOR_DPS * 0.2));
                        continue;
                    }
                    let mut best_build: Option<(usize, f32)> = None;
                    for (bidx, bowner, bx, by, _bhp, _bkind) in &buildings_snapshot {
                        if *bowner == *owner { continue; }
                        let dx = *bx as f32 * 16.0 + 8.0 - ux;
                        let dy = *by as f32 * 16.0 + 8.0 - uy;
                        let dist = (dx*dx + dy*dy).sqrt();
                        if dist < WARRIOR_RANGE && (best_build.is_none() || dist < best_build.unwrap().1) {
                            best_build = Some((*bidx, dist));
                        }
                    }
                    if let Some((bidx, _)) = best_build {
                        building_damage.push((bidx, WARRIOR_DPS * 0.2));
                    }
                }

                // Apply warrior damage
                {
                    let mut gs = state_clone.lock().await;
                    if !unit_damage.is_empty() {
                        unit_damage.sort_by_key(|(_, idx, _)| std::cmp::Reverse(*idx));
                        for (pid, idx, dmg) in unit_damage {
                            if let Some(us) = gs.units.get_mut(&pid) {
                                if idx < us.len() {
                                    let u = &mut us[idx];
                                    u.hp -= dmg;
                                    if u.hp <= 0.0 {
                                        us.remove(idx);
                                        unit_deaths.push((pid, idx));
                                        pop_updates.push(pid);
                                    } else {
                                        unit_hp_updates.push((pid, idx, u.hp));
                                    }
                                }
                            }
                        }
                    }
                    if !building_damage.is_empty() {
                        building_damage.sort_by_key(|(idx, _)| std::cmp::Reverse(*idx));
                        for (bidx, dmg) in building_damage {
                            if bidx < gs.buildings.len() {
                                let b = &mut gs.buildings[bidx];
                                b.hp -= dmg;
                                if b.hp <= 0.0 {
                                    let dead = gs.buildings.remove(bidx);
                                    building_deaths.push((dead.tile_x, dead.tile_y, dead.owner_id));
                                    if dead.kind == 3 {
                                        let cap = gs.pop_cap.entry(dead.owner_id).or_insert(default_pop_cap());
                                        *cap = (*cap - POP_FROM_HOUSE).max(default_pop_cap());
                                    }
                                } else {
                                    building_hp_updates.push((b.tile_x, b.tile_y, b.hp));
                                }
                            }
                        }
                    }
                }

                // Tower shots using snapshots
                for (owner, tx, ty) in towers_snapshot {
                    let mut best: Option<(f32, f32, f32)> = None; // dist, x, y
                    for (pid, _idx, ux, uy, _kind) in &units_snapshot {
                        if pid == &owner { continue; }
                        let dx = ux - tx;
                        let dy = uy - ty;
                        let dist = (dx*dx + dy*dy).sqrt();
                        if dist < 120.0 {
                            if best.map_or(true, |(bd, _, _)| dist < bd) {
                                best = Some((dist, *ux, *uy));
                            }
                        }
                    }
                    if let Some((_d, txp, typ)) = best {
                        shots.push((tx, ty, txp, typ, owner));
                    }
                }

                for task in to_spawn {
                    // Broadcast building spawn (memory-only ID)
                    let id = rand::random::<i32>().abs();

                    let msg = GameMessage::BuildingSpawned {
                        building: BuildingDTO {
                            id,
                            owner_id: task.owner_id,
                            kind: task.kind,
                            tile_x: task.tile_x,
                            tile_y: task.tile_y,
                            hp: hp_for_kind(task.kind),
                        }
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = tx_clone.send(json);
                    }

                    // Cache building
                            let mut gs = state_clone.lock().await;
                            gs.buildings.push(BuildingDTO {
                                id,
                                owner_id: task.owner_id,
                                kind: task.kind,
                                tile_x: task.tile_x,
                                tile_y: task.tile_y,
                                hp: hp_for_kind(task.kind),
                            });
                }

                // Refund canceled builds (no worker present). Only walls are canceled above, but handle generically.
                for task in canceled_builds {
                    let cost = cost_for_kind(task.kind);
                    if let Ok(mut gs) = state_clone.try_lock() {
                        let entry = gs.resources.entry(task.owner_id).or_insert(default_resources());
                        entry.wood += cost.wood;
                        entry.stone += cost.stone;
                        entry.gold += cost.gold;
                        entry.food += cost.food;
                        gs.building_progress.remove(&(task.tile_x, task.tile_y));
                    }
                    let gs = state_clone.lock().await;
                    let res = *gs.resources.get(&task.owner_id).unwrap_or(&default_resources());
                    let pop_cap = *gs.pop_cap.get(&task.owner_id).unwrap_or(&default_pop_cap());
                    let pop_used = gs.units.get(&task.owner_id).map(|u| u.len() as i32).unwrap_or(0);
                    let _ = tx_clone.send(serde_json::to_string(&GameMessage::ResourceUpdate {
                        player_id: task.owner_id,
                        resources: res,
                        pop_cap,
                        pop_used,
                    }).unwrap_or_default());
                }

                for task in to_spawn_units {
                    // Acquire lock once
                    let mut gs = state_clone.lock().await;
                    let units = gs.units.entry(task.owner_id).or_insert(Vec::new());
                    let next_idx = units.len();
                    
                    let tile_size = 16.0;
                    let chunk_size = 32.0;
                    let mid = chunk_size / 2.0;
                    let tc_tile_x = task.chunk_x as f32 * chunk_size + mid;
                    let tc_tile_y = task.chunk_y as f32 * chunk_size + mid;
                    
                    let col = (next_idx % 3) as f32;
                    let row = (next_idx / 3) as f32;
                    let spawn_x = (tc_tile_x * tile_size) + (col * tile_size);
                    let spawn_y = (tc_tile_y * tile_size) + tile_size * 2.0 + (row * tile_size);
                    
                    // Update Memory
                    units.push(UnitState { x: spawn_x, y: spawn_y, hp: WORKER_HP, kind: task.kind, carry_wood: 0.0, carry_stone: 0.0, carry_gold: 0.0, carry_food: 0.0 });
                    drop(gs); // Release lock before await DB/Send

                    // Broadcast
                    let msg = GameMessage::UnitSpawned {
                        unit: UnitDTO {
                            owner_id: task.owner_id,
                            unit_idx: next_idx,
                            x: spawn_x,
                            y: spawn_y,
                            kind: task.kind,
                            hp: WORKER_HP,
                            carry_wood: 0.0,
                            carry_stone: 0.0,
                            carry_gold: 0.0,
                            carry_food: 0.0,
                        }
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = tx_clone.send(json);
                    }
                }

                for (pid, res, cap, used) in resource_updates {
                    if let Ok(json) = serde_json::to_string(&GameMessage::ResourceUpdate {
                        player_id: pid,
                        resources: res,
                        pop_cap: cap,
                        pop_used: used,
                    }) {
                        let _ = tx_clone.send(json);
                    }
                }

                for (pid, idx, hp) in unit_hp_updates {
                    if let Ok(json) = serde_json::to_string(&GameMessage::UnitHp { owner_id: pid, unit_idx: idx, hp }) {
                        let _ = tx_clone.send(json);
                    }
                }
                for (pid, idx) in unit_deaths {
                    if let Ok(json) = serde_json::to_string(&GameMessage::UnitDied { owner_id: pid, unit_idx: idx }) {
                        let _ = tx_clone.send(json);
                    }
                    pop_updates.push(pid);
                }
                // Broadcast pop/resource updates after deaths
                {
                    // Use try_lock for pop updates
                    if let Ok(gs) = state_clone.try_lock() {
                        for owner in pop_updates {
                            let pop_used = gs.units.get(&owner).map(|u| u.len() as i32).unwrap_or(0);
                            let pop_cap = *gs.pop_cap.get(&owner).unwrap_or(&default_pop_cap());
                            let res = *gs.resources.get(&owner).unwrap_or(&default_resources());
                            if let Ok(json) = serde_json::to_string(&GameMessage::ResourceUpdate {
                                player_id: owner,
                                resources: res,
                                pop_cap,
                                pop_used,
                            }) {
                                let _ = tx_clone.send(json);
                            }
                        }
                    }
                }
                for (txi, tyi, hp) in building_hp_updates {
                    if let Ok(json) = serde_json::to_string(&GameMessage::BuildingHp { tile_x: txi, tile_y: tyi, hp }) {
                        let _ = tx_clone.send(json);
                    }
                }
                for (txi, tyi, _owner) in building_deaths {
                    if let Ok(json) = serde_json::to_string(&GameMessage::BuildingDestroyed { tile_x: txi, tile_y: tyi }) {
                        let _ = tx_clone.send(json);
                    }
                }

                for (sx, sy, txp, typ, owner) in shots {
                    // Apply damage to nearest target (units prioritized)
                    let mut gs = state_clone.lock().await;
                    let mut hit_unit: Option<(i32, usize)> = None;
                    let mut hit_building: Option<usize> = None;
                    let mut best_dist = 999999.0;
                    for (pid, units) in gs.units.iter_mut() {
                        if *pid == owner { continue; }
                        for (idx, u) in units.iter_mut().enumerate() {
                            let dx = u.x - txp;
                            let dy = u.y - typ;
                            let dist = (dx*dx + dy*dy).sqrt();
                            if dist < 16.0 && dist < best_dist {
                                best_dist = dist;
                                hit_unit = Some((*pid, idx));
                            }
                        }
                    }
                    if hit_unit.is_none() {
                        for (idx, b2) in gs.buildings.iter_mut().enumerate() {
                            let dx = b2.tile_x as f32 * 16.0 + 8.0 - txp;
                            let dy = b2.tile_y as f32 * 16.0 + 8.0 - typ;
                            let dist = (dx*dx + dy*dy).sqrt();
                            if dist < 16.0 && dist < best_dist && b2.owner_id != owner {
                                best_dist = dist;
                                hit_building = Some(idx);
                            }
                        }
                    }

                    if let Some((pid, idx)) = hit_unit {
                        if let Some(units) = gs.units.get_mut(&pid) {
                            if idx < units.len() {
                                let u = &mut units[idx];
                                u.hp -= TOWER_DAMAGE;
                                if let Ok(json) = serde_json::to_string(&GameMessage::UnitHp { owner_id: pid, unit_idx: idx, hp: u.hp }) {
                                    let _ = tx_clone.send(json);
                                }
                                if u.hp <= 0.0 {
                                    units.remove(idx);
                                    let _ = tx_clone.send(serde_json::to_string(&GameMessage::UnitDied { owner_id: pid, unit_idx: idx }).unwrap());
                                }
                            }
                        }
                    } else if let Some(idx) = hit_building {
                        if idx < gs.buildings.len() {
                            let b = &mut gs.buildings[idx];
                            b.hp -= TOWER_DAMAGE;
                            if let Ok(json) = serde_json::to_string(&GameMessage::BuildingHp { tile_x: b.tile_x, tile_y: b.tile_y, hp: b.hp }) {
                                let _ = tx_clone.send(json);
                            }
                            if b.hp <= 0.0 {
                                let dead = gs.buildings.remove(idx);
                                if let Ok(json) = serde_json::to_string(&GameMessage::BuildingDestroyed { tile_x: dead.tile_x, tile_y: dead.tile_y }) {
                                    let _ = tx_clone.send(json);
                                }
                                if dead.kind == 3 {
                                    let cap = gs.pop_cap.entry(dead.owner_id).or_insert(default_pop_cap());
                                    *cap = (*cap - POP_FROM_HOUSE).max(default_pop_cap());
                                }
                            }
                        }
                    }

                    if let Ok(json) = serde_json::to_string(&GameMessage::TowerShot { x1: sx, y1: sy, x2: txp, y2: typ }) {
                        let _ = tx_clone.send(json);
                    }
                }
                // println!("[TRACE] Tick {} Done", tick_count);
            }
        });
    }

    let listener = TcpListener::bind(&addr).await.expect("Failed to bind");
    println!("Listening on: {}", addr);

    // #region agent log
    /*
    {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(r"c:\25\dec-25\temty\.cursor\debug.log") {
            let _ = writeln!(file, "{{\"timestamp\":{},\"location\":\"server/src/main.rs:main\",\"message\":\"Server started listening\",\"data\":{{\"addr\":\"{}\"}},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"A\"}}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(), addr);
        }
    }
    */
    // #endregion agent log

    while let Ok((stream, _)) = listener.accept().await {
        let tx = tx.clone();
        let state = state.clone();
        tokio::spawn(handle_connection(stream, tx, state));
    }
}

async fn handle_connection(
    stream: TcpStream, 
    tx: broadcast::Sender<String>, 
    state: Arc<Mutex<GlobalState>>
) {
    let peer = stream.peer_addr().ok();
    println!("Incoming socket from {:?}", peer);

    // #region agent log
    /*
    {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(r"c:\25\dec-25\temty\.cursor\debug.log") {
            let _ = writeln!(file, "{{\"timestamp\":{},\"location\":\"server/src/main.rs:handle_connection\",\"message\":\"Incoming connection\",\"data\":{{\"peer\":\"{:?}\"}},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"A\"}}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(), peer);
        }
    }
    */
    // #endregion agent log

    // Short timeout to avoid hanging on plain HTTP probes
    let ws_stream = match timeout(Duration::from_secs(2), accept_async(stream)).await {
        Ok(Ok(ws)) => ws,
        Ok(Err(e)) => {
            println!("Error during the websocket handshake occurred from {:?}: {}", peer, e);
            return;
        }
        Err(_) => {
            println!("Handshake timeout from {:?}, closing.", peer);
            return;
        }
    };

    let (mut write, mut read) = ws_stream.split();
    let mut rx = tx.subscribe();

    // --- HANDSHAKE ---
    let client_token: Option<String>;

    if let Some(Ok(msg)) = read.next().await {
        if let Ok(text) = msg.to_text() {
            // #region agent log
            /*
            {
                use std::io::Write;
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(r"c:\25\dec-25\temty\.cursor\debug.log") {
                     let _ = writeln!(file, "{{\"timestamp\":{},\"location\":\"server/src/main.rs:handshake\",\"message\":\"Received handshake text\",\"data\":{{\"text\":\"{}\"}},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"B\"}}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(), text.replace("\"", "\\\""));
                }
            }
            */
            // #endregion agent log

            if let Ok(GameMessage::Join { version, token }) = serde_json::from_str(text) {
                
                // CHECK VERSION (memory-only)
                let required_version = MIN_CLIENT_VERSION_DEFAULT;

                if version < required_version {
                    println!("Rejecting client {:?}: version {} < required {}", peer, version, required_version);
                    let _ = write.send(Message::Text(serde_json::to_string(&GameMessage::Error { 
                        message: format!("Client version {} is too old. Minimum required: {}", version, required_version) 
                    }).unwrap())).await;
                    return;
                }
                client_token = token;
                println!("Accepted handshake from {:?}, version {}", peer, version);
            } else {
                 println!("Invalid handshake from {:?}: {}", peer, text);
                 let _ = write.send(Message::Text(serde_json::to_string(&GameMessage::Error { 
                        message: "Invalid handshake: expected Join message".to_string() 
                    }).unwrap())).await;
                return;
            }
        } else {
            return; 
        }
    } else {
        return; 
    }

    // Authenticate or Register (in-memory only)
    let (player_id, chunk_x, chunk_y, token, res_from_db) = {
        let mut gs = state.lock().await;
        if let Some(t) = client_token.as_ref() {
            if let Some(&pid) = gs.tokens.get(t) {
                let (cx, cy) = GlobalState::assign_next_position(pid);
                (pid, cx, cy, t.clone(), None)
            } else {
                let id = gs.next_id;
                gs.next_id += 1;
                let (cx, cy) = GlobalState::assign_next_position(id);
                let new_token = Uuid::new_v4().to_string();
                gs.tokens.insert(new_token.clone(), id);
                (id, cx, cy, new_token, None)
            }
        } else {
            let id = gs.next_id;
            gs.next_id += 1;
            let (cx, cy) = GlobalState::assign_next_position(id);
            let new_token = Uuid::new_v4().to_string();
            gs.tokens.insert(new_token.clone(), id);
            (id, cx, cy, new_token, None)
        }
    };

    // Memory mode: units will be ensured in the main state lock below
    let _my_units: Vec<UnitState> = Vec::new();

    // No DB fetch of offline players; will rely on in-memory state
    // Legacy DB placeholders (unused in memory mode)
    let (_db_players, _db_units, _db_buildings) = (None::<Vec<PlayerInfo>>, None::<Vec<UnitDTO>>, None::<Vec<BuildingDTO>>);

    // DB path removed; town center handled in-memory below

    // Update Global State (Active Players & Units)
    let (all_players, all_units_dto, all_buildings_dto) = {
        // Use try_lock loop to avoid blocking the thread if contended
        // WARNING: Cannot await while holding (or potentially holding in match arm) a MutexGuard!
        // We must loop and only grab guard when we succeed, immediately using it and breaking.
        // But we can't break with the guard because we need to use it.
        // We need to move the yield outside.
        // Replaced loop with async lock
        let mut gs = state.lock().await;
        // println!("[DEBUG] HandleConn Locked");
        
        gs.players.insert(player_id, PlayerInfo { id: player_id, chunk_x, chunk_y });

        // Initialize economy if missing, prefer DB-loaded resources, and ensure minimum starting resources
        {
            let entry = gs.resources.entry(player_id).or_insert(res_from_db.unwrap_or(default_resources()));
            entry.wood = entry.wood.max(MIN_START_RES.wood);
            entry.stone = entry.stone.max(MIN_START_RES.stone);
            entry.gold = entry.gold.max(MIN_START_RES.gold);
            entry.food = entry.food.max(MIN_START_RES.food);
        }
        gs.pop_cap.entry(player_id).or_insert(default_pop_cap());
        
        // Handle Units (memory only)
        if !gs.units.contains_key(&player_id) {
            gs.units.insert(player_id, GlobalState::spawn_units(chunk_x, chunk_y));
        }

        // Ensure Town Center exists (memory mode or cache for DB)
        let has_tc = gs.buildings.iter().any(|b| b.owner_id == player_id && b.kind == 0);
        if !has_tc {
            let tc_tx = chunk_x * 32 + 16;
            let tc_ty = chunk_y * 32 + 16;
            let tc = BuildingDTO {
                id: rand::random::<i32>().abs(),
                owner_id: player_id,
                kind: 0,
                tile_x: tc_tx,
                tile_y: tc_ty,
                hp: TOWN_HP,
            };
            gs.buildings.push(tc);
        }
        
        let existing_players: Vec<PlayerInfo> = gs.players.values().cloned().collect();
        
            let mut units_dto = Vec::new();
            for (pid, units) in &gs.units {
                for (i, u) in units.iter().enumerate() {
                    units_dto.push(UnitDTO {
                        owner_id: *pid,
                        unit_idx: i,
                        x: u.x,
                        y: u.y,
                        kind: u.kind,
                        hp: u.hp,
                        carry_wood: u.carry_wood,
                        carry_stone: u.carry_stone,
                        carry_gold: u.carry_gold,
                        carry_food: u.carry_food,
                    });
                }
            }
            let buildings_dto = gs.buildings.clone();
            
            (existing_players, units_dto, buildings_dto)
    };

    // Adjust pop cap for existing houses for this player
    {
        let mut gs = state.lock().await;
        let house_count = all_buildings_dto.iter().filter(|b| b.owner_id == player_id && b.kind == 3).count() as i32;
        let entry = gs.pop_cap.entry(player_id).or_insert(default_pop_cap());
        *entry = default_pop_cap() + house_count;
        // Cache buildings
        if gs.buildings.is_empty() {
            gs.buildings.extend(all_buildings_dto.clone());
        }
    }

    println!("Player {} connected (Chunk {}, {})", player_id, chunk_x, chunk_y);

    // Send Welcome
    println!("[TRACE] Sending Welcome. Units: {}, Buildings: {}", all_units_dto.len(), all_buildings_dto.len());
    
    // Prepare data without inline locking
    let (res, p_cap, p_used) = {
        let gs = state.lock().await;
        (
            *gs.resources.get(&player_id).unwrap_or(&default_resources()),
            *gs.pop_cap.get(&player_id).unwrap_or(&default_pop_cap()),
            gs.units.get(&player_id).map(|u| u.len() as i32).unwrap_or(0)
        )
    };

    let welcome_msg = serde_json::to_string(&GameMessage::Welcome {
        player_id,
        chunk_x,
        chunk_y,
        players: all_players,
        units: all_units_dto,
        buildings: all_buildings_dto,
        token: token.clone(),
        resources: res,
        pop_cap: p_cap,
        pop_used: p_used,
    }).unwrap();
    
    if let Err(e) = write.send(Message::Text(welcome_msg)).await {
        println!("Failed to send welcome: {}", e);
        return;
    }
    // println!("[TRACE] Welcome Sent");

    // Broadcast New Player
    let new_player_msg = serde_json::to_string(&GameMessage::NewPlayer {
        player: PlayerInfo { id: player_id, chunk_x, chunk_y }
    }).unwrap();
    let _ = tx.send(new_player_msg);
    // println!("[TRACE] NewPlayer Broadcast");

    // Broadcast initial resources to self (and others)
    {
        let gs = state.lock().await;
        if let Some(res) = gs.resources.get(&player_id) {
            let pop_cap = *gs.pop_cap.get(&player_id).unwrap_or(&default_pop_cap());
            let pop_used = gs.units.get(&player_id).map(|u| u.len() as i32).unwrap_or(0);
            let res_msg = GameMessage::ResourceUpdate { player_id, resources: *res, pop_cap, pop_used };
            if let Ok(json) = serde_json::to_string(&res_msg) {
                let _ = tx.send(json);
            }
        }
    }
    // println!("[TRACE] Resources Broadcast");


    // Heartbeat
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10)); // Reduced to 10s for better keepalive

    let mut send_task = tokio::spawn(async move {
        // println!("[TRACE] Send Task Start");
        loop {
            tokio::select! {
                Ok(msg) = rx.recv() => {
                    if write.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    // Send Ping
                    if write.send(Message::Ping(vec![])).await.is_err() {
                        break;
                    }
                    // Occasionally send Version Check (every minute roughly, interval is 20s, so every 3rd tick)
                    // Actually, let's just piggyback or make a dedicated message type?
                    // Simplest: Send a custom "Ping" text message or reuse Error if mismatched?
                    // Better: Just enforce it on Join.
                    // BUT user is asking for periodic checks.
                    
                    // Let's send a JSON message that the client can check silently.
                    // We don't have a specific "KeepAlive" JSON message, but we can use a new type or just trust the Ping?
                    // Pings are opcode 0x9, handled by browser engine automatically, not JS code usually.
                    
                    // Let's add a VersionCheck message to the protocol for robustness?
                    // Or just re-send Welcome? No.
                    
                    // Let's assume the "Error" message is the kick mechanism.
                    // If the server updates while players are connected, they are already "in".
                    // Do we want to KICK them?
                    // Yes, if the user deployed a breaking change (min_version bumped).
                    
                    // So we need to check current connection against MIN_VERSION.
                    // But we don't store their version in state... we just checked it at handshake.
                    // Issue: If server restarts (new binary), all clients disconnect.
                    // When they reconnect, they send their OLD version in Join.
                    // The new server checks it, sees it's old, and sends Error.
                    // The client receives Error and shows overlay.
                    
                    // So the mechanism ALREADY exists for restarts.
                    // The user is saying "it's not disabling".
                    // Maybe because the server didn't restart? Or client didn't refresh?
                    
                    // If the server updated MIN_VERSION, it means the server RESTARTED.
                    // If server restarted, connections dropped.
                    // Clients auto-reconnect?
                    // Our client code DOES NOT have auto-reconnect logic in `start()`.
                    // It just says "Chat connect failed" or "Server Error".
                    
                    // Ah, `new WebSocket()` throws if it fails.
                    // But if it closes? `ws.onclose`?
                    // We don't handle `onclose` to reload.
                    
                    // Let's add `onclose` handling to the client to auto-reload or at least alert.
                }
            }
        }
    });

    let recv_state = state.clone();

    let mut recv_task = tokio::spawn(async move {
        println!("RecvTask started for player {}", player_id);
        while let Some(Ok(msg)) = read.next().await {
            if msg.is_text() {
                let text = msg.to_text().unwrap();
                
                // Update Server State if UnitMove
                if let Ok(msg) = serde_json::from_str::<GameMessage>(text) {
                    match msg {
                        GameMessage::UnitMove { player_id, unit_idx, x, y } => {
                            // 1. Update Memory State (Fast for broadcasts)
                            {
                                // Use try_lock to avoid blocking recv loop
                                if let Ok(mut gs) = recv_state.try_lock() {
                                    if let Some(units) = gs.units.get_mut(&player_id) {
                                        if unit_idx < units.len() {
                                            units[unit_idx].x = x;
                                            units[unit_idx].y = y;
                                        }
                                    }
                                }
                            }
                            let _ = tx.send(text.to_string());
                        },
                        GameMessage::UnitSync { player_id, unit_idx, x, y } => {
                            // 1. Update Memory
                            {
                                // Use try_lock to avoid blocking recv loop
                                if let Ok(mut gs) = recv_state.try_lock() {
                                    if let Some(units) = gs.units.get_mut(&player_id) {
                                        if unit_idx < units.len() {
                                            units[unit_idx].x = x;
                                            units[unit_idx].y = y;
                                        }
                                    }
                                }
                            }
                            // 2. Broadcast
                let _ = tx.send(text.to_string());
                            
                        },
                        GameMessage::Build { kind, tile_x, tile_y } => {
                            // Resource check and simple tile occupancy check
                            // Use try_lock to avoid blocking
                            if let Ok(mut gs) = recv_state.try_lock() {
                                if gs.is_tile_blocked(tile_x, tile_y) {
                                    continue;
                                }
                                let cost = cost_for_kind(kind);
                                let entry = gs.resources.entry(player_id).or_insert(default_resources());
                                if !entry.spend(&cost) {
                                    continue;
                                }
                                // Update pop cap if house built
                                if kind == 3 {
                                    let cap = gs.pop_cap.entry(player_id).or_insert(default_pop_cap());
                                    *cap += POP_FROM_HOUSE;
                                }
                                // Track progress start
                                gs.building_progress.insert((tile_x, tile_y), BuildTask { owner_id: player_id, kind, tile_x, tile_y, progress: 0.0 });
                            } else {
                                continue; // Skip build if contended
                            }

                            // Broadcast initial progress
                            if let Ok(json) = serde_json::to_string(&GameMessage::BuildProgress { tile_x, tile_y, kind, progress: 0.0 }) {
                                let _ = tx.send(json);
                            }

                            // Resource update broadcast
                            let gs = recv_state.lock().await;
                            if let Some(res) = gs.resources.get(&player_id) {
                                let pop_cap = *gs.pop_cap.get(&player_id).unwrap_or(&default_pop_cap());
                                let pop_used = gs.units.get(&player_id).map(|u| u.len() as i32).unwrap_or(0);
                                let res_msg = GameMessage::ResourceUpdate { player_id, resources: *res, pop_cap, pop_used };
                                if let Ok(json) = serde_json::to_string(&res_msg) {
                                    let _ = tx.send(json);
                                }
                            }
                        },
                        GameMessage::AssignGather { unit_ids, target_x, target_y, kind } => {
                            if let Ok(mut gs) = recv_state.try_lock() {
                                for uid in unit_ids {
                                    gs.gather_tasks.insert((player_id, uid), GatherTask { kind, target_x, target_y });
                                }
                            }
                        },
                        GameMessage::TrainUnit { building_id, kind } => {
                            // ... (existing code) ...
                            // To match 'new_string' block requirements, I will just paste the handling logic for delete messages after this block in a separate replace call or try to fit it if context allows.
                            // Actually, I should use a separate replace for the match arm insertion to be safe.
                            // But I need to match the 'match msg {' block structure.

                            // Only warrior supported for now
                            if kind != 1 { continue; }
                            let building = {
                                if let Ok(gs) = recv_state.try_lock() {
                                    gs.find_building(player_id, building_id)
                                } else {
                                    None
                                }
                            };
                            if building.is_none() { continue; }
                            let pop_cap = {
                                if let Ok(gs) = recv_state.try_lock() {
                                    *gs.pop_cap.get(&player_id).unwrap_or(&default_pop_cap())
                                } else {
                                    0
                                }
                            };
                            let pop_used = {
                                if let Ok(gs) = recv_state.try_lock() {
                                    gs.units.get(&player_id).map(|u| u.len() as i32).unwrap_or(0)
                                } else {
                                    0
                                }
                            };
                            if pop_used >= pop_cap { continue; }

                            // Resource check
                            {
                                if let Ok(mut gs) = recv_state.try_lock() {
                                    let entry = gs.resources.entry(player_id).or_insert(default_resources());
                                    if !entry.spend(&COST_WARRIOR) {
                                        continue;
                                    }
                                } else {
                                    continue;
                                }
                            }

                            // Compute spawn position near building
                            let b = building.unwrap();
                            let tile_size = 16.0;
                            let spawn_x = b.tile_x as f32 * tile_size + tile_size;
                            let spawn_y = b.tile_y as f32 * tile_size + tile_size * 0.5;

                            // Persist and broadcast
                            let next_idx = {
                                if let Ok(mut gs) = recv_state.try_lock() {
                                    let entry = gs.units.entry(player_id).or_insert(Vec::new());
                                    let idx = entry.len();
                                    entry.push(UnitState { x: spawn_x, y: spawn_y, hp: WARRIOR_HP, kind: 1, carry_wood: 0.0, carry_stone: 0.0, carry_gold: 0.0, carry_food: 0.0 });
                                    idx
                                } else {
                                    0 // Fail gracefully
                                }
                            };

                            let new_unit_msg = serde_json::to_string(&GameMessage::UnitSpawned {
                                unit: UnitDTO {
                                    owner_id: player_id,
                                    unit_idx: next_idx,
                                    x: spawn_x,
                                    y: spawn_y,
                                    kind: 1,
                                    hp: WARRIOR_HP,
                                    carry_wood: 0.0,
                                    carry_stone: 0.0,
                                    carry_gold: 0.0,
                                    carry_food: 0.0,
                                }
                            }).unwrap();
                            let _ = tx.send(new_unit_msg);

                            // Resource update
                            let gs = recv_state.lock().await;
                            if let Some(res) = gs.resources.get(&player_id) {
                                let pop_cap = *gs.pop_cap.get(&player_id).unwrap_or(&default_pop_cap());
                                let pop_used = gs.units.get(&player_id).map(|u| u.len() as i32).unwrap_or(0);
                                let res_msg = GameMessage::ResourceUpdate { player_id, resources: *res, pop_cap, pop_used };
                                if let Ok(json) = serde_json::to_string(&res_msg) {
                                    let _ = tx.send(json);
                                }
                            }
                        },
                        GameMessage::SpawnUnit => {
                            // Handle Spawn
                            let (chunk_x, chunk_y, unit_count) = {
                                if let Ok(mut gs) = recv_state.try_lock() {
                                    // Separate borrows: First get player coords (values only)
                                    let player_coords = gs.players.get(&player_id).map(|p| (p.chunk_x, p.chunk_y));
                                    
                                    if let Some((cx, cy)) = player_coords {
                                        // Now mutable borrow is safe
                                        let units = gs.units.entry(player_id).or_insert(Vec::new());
                                        (cx, cy, units.len())
                                    } else {
                                        (0, 0, 0)
                                    }
                                } else {
                                    (0, 0, 0)
                                }
                            };
                            
                            // LIMIT: Max 5 workers
                            let pop_cap = {
                                if let Ok(gs) = recv_state.try_lock() {
                                    *gs.pop_cap.get(&player_id).unwrap_or(&default_pop_cap())
                                } else {
                                    0
                                }
                            };
                            if unit_count as i32 >= pop_cap {
                                continue;
                            }

                            // Resource check
                            {
                                if let Ok(mut gs) = recv_state.try_lock() {
                                    let entry = gs.resources.entry(player_id).or_insert(default_resources());
                                    if !entry.spend(&COST_WORKER) {
                                        continue;
                                    }
                                } else {
                                    continue;
                                }
                            }
                            // INSTANT SPAWN (no training delay)
                            let mut spawned: Option<(f32, f32, usize)> = None;
                            {
                                if let Ok(mut gs) = recv_state.try_lock() {
                                    let units = gs.units.entry(player_id).or_insert(Vec::new());
                                    let next_idx = units.len();
                                    
                                    let tile_size = 16.0;
                                    let chunk_size = 32.0;
                                    let mid = chunk_size / 2.0;
                                    let tc_tile_x = chunk_x as f32 * chunk_size + mid;
                                    let tc_tile_y = chunk_y as f32 * chunk_size + mid;
                                    
                                    let col = (next_idx % 3) as f32;
                                    let row = (next_idx / 3) as f32;
                                    let spawn_x = (tc_tile_x * tile_size) + (col * tile_size);
                                    let spawn_y = (tc_tile_y * tile_size) + tile_size * 2.0 + (row * tile_size);
                                    
                                    units.push(UnitState { x: spawn_x, y: spawn_y, hp: WORKER_HP, kind: 0, carry_wood: 0.0, carry_stone: 0.0, carry_gold: 0.0, carry_food: 0.0 });
                                    spawned = Some((spawn_x, spawn_y, next_idx));
                                }
                            }

                            if let Some((spawn_x, spawn_y, next_idx)) = spawned {
                                // Persist to DB
                                // Broadcast spawn
                                let msg = GameMessage::UnitSpawned {
                                    unit: UnitDTO {
                                        owner_id: player_id,
                                        unit_idx: next_idx,
                                        x: spawn_x,
                                        y: spawn_y,
                                        kind: 0,
                                        hp: WORKER_HP,
                                        carry_wood: 0.0,
                                        carry_stone: 0.0,
                                        carry_gold: 0.0,
                                        carry_food: 0.0,
                                    }
                                };
                                if let Ok(json) = serde_json::to_string(&msg) {
                                    let _ = tx.send(json);
                                }

                                // Broadcast resource update (already spent; include updated pop_used)
                                let mut res_msg = None;
                                if let Ok(gs) = recv_state.try_lock() {
                                    if let Some(res) = gs.resources.get(&player_id) {
                                        let pop_cap = *gs.pop_cap.get(&player_id).unwrap_or(&default_pop_cap());
                                        let pop_used = gs.units.get(&player_id).map(|u| u.len() as i32).unwrap_or(0);
                                        res_msg = Some(GameMessage::ResourceUpdate { player_id, resources: *res, pop_cap, pop_used });
                                    }
                                }
                                if let Some(msg) = res_msg {
                                    if let Ok(json) = serde_json::to_string(&msg) {
                                        let _ = tx.send(json);
                                    }
                                }
                            }
                        },
                        GameMessage::DeleteUnit { unit_idx } => {
                            let mut pid_to_update = None;
                            if let Ok(mut gs) = recv_state.try_lock() {
                                let mut refund_food = false;
                                {
                                    if let Some(units) = gs.units.get_mut(&player_id) {
                                        // Unit idx in message is the player-specific index
                                        if unit_idx < units.len() {
                                            refund_food = units[unit_idx].kind == 0; // Worker
                                            units.remove(unit_idx);
                                            pid_to_update = Some(player_id);
                                        }
                                    }
                                }

                                if refund_food {
                                    if let Some(res) = gs.resources.get_mut(&player_id) {
                                        res.food += COST_WORKER.food;
                                    }
                                }
                            }
                            
                            if let Some(pid) = pid_to_update {
                                // DB Update
                                // Broadcast Death
                                let _ = tx.send(serde_json::to_string(&GameMessage::UnitDied { owner_id: pid, unit_idx }).unwrap());
                                
                                // Broadcast Resource/Pop Update
                                let gs = recv_state.lock().await;
                                let pop_used = gs.units.get(&pid).map(|u| u.len() as i32).unwrap_or(0);
                                let pop_cap = *gs.pop_cap.get(&pid).unwrap_or(&default_pop_cap());
                                let res = *gs.resources.get(&pid).unwrap_or(&default_resources());
                                let _ = tx.send(serde_json::to_string(&GameMessage::ResourceUpdate { 
                                    player_id: pid, resources: res, pop_cap, pop_used 
                                }).unwrap());
                            }
                        },
                        GameMessage::DeleteBuilding { tile_x, tile_y } => {
                            let mut destroyed = false;
                            let mut owner = 0;
                            let mut refund = default_resources();
                            let mut do_refund = false;
                            
                            {
                                if let Ok(mut gs) = recv_state.try_lock() {
                                    if let Some(idx) = gs.buildings.iter().position(|b| b.tile_x == tile_x && b.tile_y == tile_y) {
                                        let b = gs.buildings[idx].clone();
                                        if b.owner_id == player_id && b.kind != 0 { // Cannot delete Town Center (kind 0)
                                            owner = b.owner_id;
                                            let is_house = b.kind == 3;
                                            refund = cost_for_kind(b.kind);
                                            do_refund = true;
                                            gs.buildings.remove(idx);
                                            destroyed = true;
                                            
                                            if is_house {
                                                let cap = gs.pop_cap.entry(owner).or_insert(default_pop_cap());
                                                *cap = (*cap - POP_FROM_HOUSE).max(default_pop_cap());
                                            }
                                        }
                                    }
                                }
                            }
                            
                            if destroyed {
                                // DB Update
                                // Broadcast Destroyed
                                let _ = tx.send(serde_json::to_string(&GameMessage::BuildingDestroyed { tile_x, tile_y }).unwrap());
                                
                                // Apply refund and broadcast Resource/Pop Update
                                if do_refund {
                                    if let Ok(mut gs) = recv_state.try_lock() {
                                        let entry = gs.resources.entry(owner).or_insert(default_resources());
                                        entry.wood += refund.wood;
                                        entry.stone += refund.stone;
                                        entry.gold += refund.gold;
                                        entry.food += refund.food;
                                    }
                                }
                                let gs = recv_state.lock().await;
                                let pop_used = gs.units.get(&owner).map(|u| u.len() as i32).unwrap_or(0);
                                let pop_cap = *gs.pop_cap.get(&owner).unwrap_or(&default_pop_cap());
                                let res = *gs.resources.get(&owner).unwrap_or(&default_resources());
                                let _ = tx.send(serde_json::to_string(&GameMessage::ResourceUpdate { 
                                    player_id: owner, resources: res, pop_cap, pop_used 
                                }).unwrap());
                            }
                        },
                        _ => {}
                    }
                }
            }
        }
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };
    
    // Cleanup (keep player state in memory so positions/resources persist across reconnects)
    println!("Player {} disconnected", player_id);
}
