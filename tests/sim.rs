use cinderfront::net::{Conn, Host, C2H, H2C};
use cinderfront::sim::*;

fn run(w: &mut World, ticks: u32) {
    for _ in 0..ticks {
        w.tick();
    }
}

/// Build `kind` through the real command path, on the first valid spot near the yard.
fn build(w: &mut World, p: u8, kind: StructKind) -> (i32, i32) {
    w.apply(p, &Command::StartBuild(kind));
    assert!(w.players[p as usize].construction.is_some(), "{kind:?} did not start");
    for _ in 0..2000 {
        w.tick();
        if w.players[p as usize].construction.as_ref().unwrap().ready {
            break;
        }
    }
    let (cx, cy) = w.starts[p as usize];
    for r in 0..12 {
        for y in cy - r..=cy + r {
            for x in cx - r..=cx + r {
                if w.can_place(p, kind, x, y) {
                    w.apply(p, &Command::Place { kind, x, y });
                    assert!(w.players[p as usize].construction.is_none());
                    return (x, y);
                }
            }
        }
    }
    panic!("no spot for {kind:?}");
}

fn train(w: &mut World, p: u8, kind: UnitKind) -> u32 {
    let before: Vec<u32> = w.units.keys().copied().collect();
    w.apply(p, &Command::Train(kind));
    for _ in 0..2000 {
        w.tick();
        if let Some(u) = w.units.values().find(|u| u.kind == kind && u.owner == p && !before.contains(&u.id)) {
            return u.id;
        }
    }
    panic!("{kind:?} never trained");
}

#[test]
fn map_is_symmetric_and_connected() {
    let w = World::new(7, 4);
    for y in 0..MAP_H {
        for x in 0..MAP_W {
            assert_eq!(w.terrain[idx(x, y)], w.terrain[idx(MAP_W - 1 - x, MAP_H - 1 - y)]);
        }
    }
    // The start tiles themselves sit under the yards, so path between the tiles just south of them.
    let (a, b) = ((w.starts[0].0, w.starts[0].1 + 3), (w.starts[1].0, w.starts[1].1 + 3));
    let p = w.find_path(a, b, false);
    assert_eq!(p.first().copied(), Some(b), "starts must be connected by ground");
    let count = |t| w.terrain.iter().filter(|x| **x == t).count();
    println!(
        "dust {} rock {} ore {} cliff {}",
        count(Terrain::Dust),
        count(Terrain::Rock),
        count(Terrain::Ore),
        count(Terrain::Cliff)
    );
    for y in 0..MAP_H {
        let row: String = (0..MAP_W)
            .map(|x| match w.terrain[idx(x, y)] {
                Terrain::Dust => '.',
                Terrain::Rock => '#',
                Terrain::Ore => 'o',
                Terrain::Cliff => '^',
                Terrain::Bank => '=',
                Terrain::Ramp => '/',
            })
            .collect();
        println!("{row}");
    }
}

#[test]
fn levels_only_change_at_ramps() {
    let w = World::new(7, 4);
    let walk = |x: i32, y: i32| inb(x, y) && !matches!(w.terrain[idx(x, y)], Terrain::Cliff | Terrain::Bank);
    let mut ramps = 0;
    for y in 0..MAP_H {
        for x in 0..MAP_W {
            ramps += (w.terrain[idx(x, y)] == Terrain::Ramp) as i32;
            for (dx, dy) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
                let (a, b) = (idx(x, y), (x + dx, y + dy));
                if walk(x, y) && walk(b.0, b.1) {
                    let one_ramp = w.terrain[a] == Terrain::Ramp || w.terrain[idx(b.0, b.1)] == Terrain::Ramp;
                    let diff = (w.level[a] as i32 - w.level[idx(b.0, b.1)] as i32).abs();
                    assert!(diff == 0 || one_ramp, "level step without a ramp at {x},{y}");
                }
            }
        }
    }
    assert!(ramps >= 12, "expected home and centre ramps, found {ramps} ramp tiles");
    // Bases sit on raised ground, the centre higher still.
    let s = w.starts[0];
    assert_eq!(w.level[idx(s.0, s.1)], 1);
    assert_eq!(w.level[idx(MAP_W / 2, MAP_H / 2)], 2);
}

#[test]
fn high_ground_outranges() {
    let w = World::new(7, 2);
    let (hi, lo) = (center(w.starts[0]), center((w.starts[0].0 + 15, w.starts[0].1 + 15)));
    assert_eq!(w.level_at(hi), 1);
    assert_eq!(w.level_at(lo), 0);
    assert_eq!(w.height_bonus(hi, false, lo, false), 1);
    assert_eq!(w.height_bonus(lo, false, hi, false), 0, "no bonus shooting uphill");
    assert_eq!(w.height_bonus(hi, false, lo, true), 0, "no bonus against aircraft");
}

#[test]
fn slabs_give_full_health() {
    let mut w = World::new(7, 2);
    // Start area is pre-slabbed: first reactor lands on slab or partly off it.
    let (x, y) = build(&mut w, 0, StructKind::Power);
    let s = w.structs.values().find(|s| s.kind == StructKind::Power).unwrap();
    let d = StructKind::Power.def();
    let slabbed = (0..2).flat_map(|dy| (0..2).map(move |dx| (dx, dy))).filter(|(dx, dy)| w.slab[idx(x + dx, y + dy)]).count() as i32;
    assert_eq!(s.hp, d.hp * (50 + 50 * slabbed / 4) / 100);
}

#[test]
fn economy_army_and_airlift() {
    let mut w = World::new(7, 2);
    w.players[0].credits += 5000;
    build(&mut w, 0, StructKind::Power);
    build(&mut w, 0, StructKind::Refinery);
    let harv = w.units.values().find(|u| u.kind == UnitKind::Harvester).expect("refinery brings a harvester").id;
    let credits = w.players[0].credits;
    run(&mut w, 20 * 90);
    assert!(w.players[0].credits > credits, "harvester should have delivered ore");
    assert!(w.units.contains_key(&harv));

    build(&mut w, 0, StructKind::Power);
    build(&mut w, 0, StructKind::Factory);
    build(&mut w, 0, StructKind::Airpad);
    w.apply(0, &Command::Train(UnitKind::Tank));
    assert!(w.players[0].queues[1].items.is_empty(), "tanks are locked until the Tech Lab stands");
    build(&mut w, 0, StructKind::TechLab);
    let tank = train(&mut w, 0, UnitKind::Tank);
    let carrier = train(&mut w, 0, UnitKind::Carrier);

    // Airlift the tank right next to the enemy base, across the map.
    let (ex, ey) = w.starts[1];
    let dest = (ex - 6, ey - 6);
    w.apply(0, &Command::Airlift { units: vec![tank], x: dest.0, y: dest.1 });
    let mut was_carried = false;
    for _ in 0..20 * 120 {
        w.tick();
        was_carried |= w.units[&tank].carried;
        if was_carried && !w.units[&tank].carried {
            break;
        }
    }
    assert!(was_carried, "carrier never picked the tank up");
    let t = &w.units[&tank];
    assert!(!t.carried, "tank never dropped");
    assert_eq!(t.tile, dest);
    assert_eq!(w.occ[idx(dest.0, dest.1)], tank);
    assert!(w.units[&carrier].cargo.is_none());

    // Tank levels the enemy yard: player 1 is defeated, player 0 wins.
    let yard = w.structs.values().find(|s| s.owner == 1).unwrap().id;
    w.apply(0, &Command::Attack { units: vec![tank], target: Target::Struct(yard) });
    for _ in 0..20 * 600 {
        w.tick();
        if w.winner.is_some() {
            break;
        }
    }
    assert_eq!(w.winner, Some(0));
}

#[test]
fn same_commands_same_state() {
    let script = |w: &mut World| {
        for t in 0..3000u32 {
            if t == 1 {
                w.apply(0, &Command::StartBuild(StructKind::Power));
                w.apply(1, &Command::StartBuild(StructKind::Power));
            }
            if t == 200 {
                for p in 0..2u8 {
                    let (cx, cy) = w.starts[p as usize];
                    'f: for y in cy - 8..cy + 8 {
                        for x in cx - 8..cx + 8 {
                            if w.can_place(p, StructKind::Power, x, y) {
                                w.apply(p, &Command::Place { kind: StructKind::Power, x, y });
                                break 'f;
                            }
                        }
                    }
                }
            }
            if t == 250 {
                // Everyone marches on the map centre and fights there.
                for p in 0..2u8 {
                    let units: Vec<u32> = w.units.values().filter(|u| u.owner == p).map(|u| u.id).collect();
                    w.apply(p, &Command::Move { units, x: 47, y: 47, attack: true });
                }
            }
            w.tick();
        }
        w.hash()
    };
    let (mut a, mut b) = (World::new(7, 2), World::new(7, 2));
    let (ha, hb) = (script(&mut a), script(&mut b));
    assert_eq!(ha, hb);
    assert!(a.units.len() < 6, "armies should have met and fought (units left: {})", a.units.len());
}

#[test]
fn host_and_client_exchange_turns() {
    let mut host = Host::new(0).unwrap();
    let addr = format!("127.0.0.1:{}", host.port()).parse().unwrap();
    let mut client = Conn::connect(addr).unwrap();
    let wait = |f: &mut dyn FnMut() -> bool| {
        for _ in 0..200 {
            if f() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out");
    };
    wait(&mut || {
        host.lobby_poll();
        host.players() == 2
    });
    assert_eq!(host.start(7), 2);
    let mut got = Vec::new();
    wait(&mut || {
        got.extend(client.poll::<H2C>());
        got.iter().any(|m| matches!(m, H2C::Start { you: 1, players: 2, seed: 7 }))
    });
    client.send(&C2H::Cmd(Command::Train(UnitKind::Tank)));
    let mut cmds = Vec::new();
    wait(&mut || {
        cmds.extend(host.collect().0);
        !cmds.is_empty()
    });
    assert!(matches!(cmds[0], (1, Command::Train(UnitKind::Tank))));
    host.broadcast(&H2C::Turn { cmds });
    let mut turns = Vec::new();
    wait(&mut || {
        turns.extend(client.poll::<H2C>());
        turns.iter().any(|m| matches!(m, H2C::Turn { cmds } if cmds.len() == 1))
    });
}

#[test]
fn repair_and_sell() {
    let mut w = World::new(7, 2);
    let yard = w.structs.values().find(|s| s.owner == 0).unwrap().id;
    let full = StructKind::Yard.def().hp;
    w.structs.get_mut(&yard).unwrap().hp = full / 2;
    w.apply(1, &Command::Repair { id: yard });
    assert!(!w.structs[&yard].repairing, "cannot repair someone else's building");
    w.apply(0, &Command::Repair { id: yard });
    run(&mut w, 20 * 30);
    assert_eq!(w.structs[&yard].hp, full);
    assert!(!w.structs[&yard].repairing);

    build(&mut w, 0, StructKind::Power);
    let reactor = w.structs.values().find(|s| s.kind == StructKind::Power).unwrap();
    let (id, x, y, hp) = (reactor.id, reactor.x, reactor.y, reactor.hp);
    let before = w.players[0].credits;
    w.apply(0, &Command::Sell { id });
    assert!(!w.structs.contains_key(&id));
    assert_eq!(w.structure_at[idx(x, y)], 0);
    let d = StructKind::Power.def();
    assert_eq!(w.players[0].credits, before + d.cost / 2 * hp / d.hp);
}

#[test]
fn new_units_go_to_the_gather_point() {
    let mut w = World::new(7, 2);
    build(&mut w, 0, StructKind::Power);
    build(&mut w, 0, StructKind::Barracks);
    let id = w.structs.values().find(|s| s.kind == StructKind::Barracks).unwrap().id;
    let (cx, cy) = w.starts[0];
    let point = (cx + 6, cy + 6);
    w.apply(0, &Command::Rally { id, x: point.0, y: point.1 });
    let trooper = train(&mut w, 0, UnitKind::Trooper);
    run(&mut w, 20 * 30);
    assert_eq!(w.units[&trooper].tile, point);
}

#[test]
fn harvester_returns_to_its_assigned_patch() {
    let mut w = World::new(7, 2);
    build(&mut w, 0, StructKind::Power);
    build(&mut w, 0, StructKind::Refinery);
    let harv = w.units.values().find(|u| u.kind == UnitKind::Harvester).unwrap().id;
    // Send it to the field between base and centre, far past the patches by the base.
    let home = (35, 35);
    w.apply(0, &Command::Harvest { units: vec![harv], x: home.0, y: home.1 });
    let near_home = |w: &World| {
        let t = w.units[&harv].tile;
        (t.0 - home.0).abs() <= 8 && (t.1 - home.1).abs() <= 8
    };
    let mut trips = 0;
    let mut was_full = false;
    for _ in 0..20 * 600 {
        w.tick();
        let u = &w.units[&harv];
        if u.load == HARVEST_CAP {
            was_full = true;
        }
        // Each time it starts loading again after a delivery, it must be back at the assigned field.
        if was_full && u.load > 0 && u.load < 50 && u.order == Order::Harvest(None) {
            assert!(near_home(&w), "harvester mined at {:?} instead of its patch", u.tile);
            was_full = false;
            trips += 1;
            if trips == 2 {
                return;
            }
        }
    }
    panic!("harvester completed only {trips} return trips");
}

#[test]
fn mcv_deploys_into_a_yard_and_rocket_turret_outranges_turret() {
    let mut w = World::new(7, 2);
    let (cx, cy) = w.starts[0];
    let mcv = w.new_unit(0, UnitKind::Mcv, (cx + 5, cy));
    w.apply(0, &Command::Deploy { units: vec![mcv] });
    assert!(!w.units.contains_key(&mcv));
    assert_eq!(w.structs.values().filter(|s| s.owner == 0 && s.kind == StructKind::Yard).count(), 2);
    assert!(w.structure_at[idx(cx + 5, cy)] != 0);

    let (r, d, _) = StructKind::RocketTurret.weapon().unwrap();
    let (r0, d0, _) = StructKind::Turret.weapon().unwrap();
    assert!(r > r0 && d > d0);
    // An enemy tank 8 tiles away: out of a gun turret's reach, inside a rocket turret's.
    let rt = w.create_struct(0, StructKind::RocketTurret, cx, cy + 4, 1000);
    let tank = w.new_unit(1, UnitKind::Tank, (cx, cy + 12));
    let hp = w.units[&tank].hp;
    w.players[0].credits = 0;
    run(&mut w, 40);
    assert!(w.units[&tank].hp < hp, "rocket turret should have fired");
    assert!(w.structs.contains_key(&rt));
}

/// A clear ground tile near player 0's start, `dx` tiles east of it.
fn near_start(w: &World, dx: i32, dy: i32) -> Tile {
    let (cx, cy) = w.starts[0];
    (cx + dx, cy + dy)
}

#[test]
fn queued_orders_run_in_turn() {
    let mut w = World::new(7, 2);
    let id = w.new_unit(0, UnitKind::Buggy, near_start(&w, 0, 5));
    let (a, b) = (near_start(&w, 6, 5), near_start(&w, 6, 9));
    w.apply(0, &Command::Move { units: vec![id], x: a.0, y: a.1, attack: false });
    w.apply(0, &Command::Queue { units: vec![id], step: Step::Move(b) });
    assert_eq!(w.units[&id].queue.len(), 1, "busy unit keeps the step for later");
    let mut reached_a = false;
    for _ in 0..600 {
        w.tick();
        reached_a |= w.units[&id].tile == a;
    }
    assert!(reached_a, "first waypoint visited");
    assert_eq!(w.units[&id].tile, b, "ends at the queued waypoint");
    assert!(w.units[&id].queue.is_empty());

    // A fresh order throws the queue away.
    w.apply(0, &Command::Move { units: vec![id], x: a.0, y: a.1, attack: false });
    w.apply(0, &Command::Queue { units: vec![id], step: Step::Move(b) });
    w.apply(0, &Command::Stop { units: vec![id] });
    assert!(w.units[&id].queue.is_empty());

    // An idle unit starts a queued step straight away.
    w.apply(0, &Command::Queue { units: vec![id], step: Step::Move(a) });
    assert_eq!(w.units[&id].order, Order::Move(a));
}

#[test]
fn patrol_goes_back_and_forth() {
    let mut w = World::new(7, 2);
    let start = near_start(&w, 0, 5);
    let id = w.new_unit(0, UnitKind::Buggy, start);
    let far = near_start(&w, 7, 5);
    w.apply(0, &Command::Patrol { units: vec![id], x: far.0, y: far.1 });
    let (mut out, mut back) = (false, false);
    for _ in 0..800 {
        w.tick();
        let t = w.units[&id].tile;
        out |= t == far;
        back |= out && t == start;
    }
    assert!(out && back, "patrol should reach the far end and come back");
    assert!(matches!(w.units[&id].order, Order::Patrol { .. }), "still patrolling");
}

#[test]
fn formation_keeps_shape_and_pace() {
    let mut w = World::new(7, 2);
    let slow = w.new_unit(0, UnitKind::Tank, near_start(&w, 0, 5));
    let fast = w.new_unit(0, UnitKind::Buggy, near_start(&w, 2, 5));
    let target = near_start(&w, 1, 9);
    w.apply(0, &Command::Formation { units: vec![slow, fast], x: target.0, y: target.1, attack: false });
    let tank_speed = UnitKind::Tank.def().speed;
    assert_eq!(w.units[&fast].speed_cap, tank_speed, "buggy held to the tank's pace");
    for _ in 0..600 {
        w.tick();
    }
    let (ts, tf) = (w.units[&slow].tile, w.units[&fast].tile);
    assert_eq!(tf.0 - ts.0, 2, "side-by-side spacing kept");
    assert_eq!(tf.1, ts.1);
    assert_eq!(w.units[&fast].speed_cap, 0, "cap lifted on arrival");
}

#[test]
fn supply_caps_limit_training() {
    use cinderfront::sim::Pool;
    let mut w = World::new(7, 2);
    w.players[0].credits += 50_000;
    build(&mut w, 0, StructKind::Power);
    build(&mut w, 0, StructKind::Refinery);
    build(&mut w, 0, StructKind::Power);
    build(&mut w, 0, StructKind::Factory);
    build(&mut w, 0, StructKind::Airpad);

    // One refinery: the harvester it came with plus one more.
    assert_eq!(w.supply(0, Pool::Harvest), (1, 2));
    for _ in 0..3 {
        w.apply(0, &Command::Train(UnitKind::Harvester));
    }
    assert_eq!(w.supply(0, Pool::Harvest), (2, 2), "queued units hold their points");
    assert_eq!(w.players[0].queues[1].items.len(), 1);

    // One airpad holds 20 points: four carriers at 5 each.
    for _ in 0..6 {
        w.apply(0, &Command::Train(UnitKind::Carrier));
    }
    assert_eq!(w.players[0].queues[2].items.len(), 4);
    w.apply(0, &Command::CancelTrain(UnitKind::Carrier));
    assert_eq!(w.supply(0, Pool::Air), (15, 20));

    // A second airpad raises the cap wherever it stands.
    build(&mut w, 0, StructKind::Power);
    build(&mut w, 0, StructKind::Airpad);
    assert_eq!(w.supply(0, Pool::Air).1, 40);
}
