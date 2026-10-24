#[cfg(test)]
mod unit_tests {
    use crate::spring::Spring;
    use crate::state_machine::{GestureStateMachine, NavigationGesture};
    use crate::touch::{calculate_snap_target, FlickDirection, TouchTracker};
    use std::time::Duration;

    #[test]
    fn test_spring_settling_and_threshold() {
        let mut spring = Spring::expressive()
            .with_position(100.0)
            .with_target(0.0)
            .with_velocity(0.0);

        let dt = 1.0 / 60.0;
        let mut steps = 0;
        let max_steps = 300; // 5 seconds at 60 fps

        while steps < max_steps && !spring.is_settled(0.1, 1.0) {
            spring.step(dt);
            steps += 1;
        }

        assert!(
            spring.is_settled(0.1, 1.0),
            "Spring should settle within threshold, current pos={}, vel={}",
            spring.position,
            spring.velocity
        );
        assert!(
            steps < 120,
            "Spring should settle within ~2 seconds, took {} steps",
            steps
        );
        assert!((spring.position - spring.target).abs() < 0.1);
    }

    #[test]
    fn test_critically_damped_no_overshoot() {
        let mut spring = Spring::critically_damped()
            .with_position(0.0)
            .with_target(100.0)
            .with_velocity(0.0);

        let dt = 1.0 / 120.0;
        let mut max_pos = 0.0f64;

        for _ in 0..240 {
            let pos = spring.step(dt);
            if pos > max_pos {
                max_pos = pos;
            }
        }

        // Critically damped system starting from 0 towards 100 should not overshoot 100
        assert!(
            max_pos <= 100.001,
            "Critically damped spring overshot target: max_pos={}",
            max_pos
        );
        assert!(spring.is_settled(0.5, 2.0));
    }

    #[test]
    fn test_underdamped_oscillates() {
        let mut spring = Spring::bouncy()
            .with_position(0.0)
            .with_target(100.0)
            .with_velocity(0.0);

        let dt = 1.0 / 120.0;
        let mut overshot = false;

        for _ in 0..120 {
            let pos = spring.step(dt);
            if pos > 100.5 {
                overshot = true;
                break;
            }
        }

        assert!(
            overshot,
            "Underdamped bouncy spring should overshoot target on first pass"
        );
    }

    #[test]
    fn test_touch_tracker_velocity_and_flick() {
        let mut tracker = TouchTracker::default();
        let t0 = Duration::from_millis(1000);

        // Simulate a rapid swipe right: 0px at 0ms -> 300px at 100ms (3000 px/s)
        for i in 0..=5 {
            let elapsed_ms = i * 20;
            let x = (i as f64) * 60.0;
            let t = t0 + Duration::from_millis(elapsed_ms);
            tracker.record_sample(x, 100.0, t);
        }

        let vel = tracker.compute_velocity();
        assert!(
            vel.x > 2500.0 && vel.x < 3500.0,
            "Velocity should be approx 3000 px/s, got {}",
            vel.x
        );
        assert!(vel.y.abs() < 10.0, "Y velocity should be near zero");

        let flick = tracker.detect_flick(1000.0, 50.0);
        assert_eq!(flick, Some(FlickDirection::Right));
    }

    #[test]
    fn test_snap_target_selection() {
        let targets = [0.0, 400.0, 800.0];

        // High positive fling velocity snaps to next upper target
        let snap1 = calculate_snap_target(100.0, 1500.0, &targets, 500.0);
        assert_eq!(snap1, 400.0);

        // High negative fling velocity snaps to next lower target
        let snap2 = calculate_snap_target(350.0, -1500.0, &targets, 500.0);
        assert_eq!(snap2, 0.0);

        // Low velocity snaps to closest target
        let snap3 = calculate_snap_target(380.0, 50.0, &targets, 500.0);
        assert_eq!(snap3, 400.0);

        let snap4 = calculate_snap_target(210.0, 0.0, &targets, 500.0);
        assert_eq!(snap4, 400.0);
    }

    #[test]
    fn test_state_machine_return_home_gesture() {
        let mut sm = GestureStateMachine::default();
        let viewport_height = 915.0;

        // Start drag from bottom bar
        let t0 = Duration::from_millis(500);
        sm.on_down(200.0, 890.0, t0);

        // Quick flick up within 150ms
        sm.on_move(200.0, 800.0, t0 + Duration::from_millis(50));
        sm.on_move(200.0, 700.0, t0 + Duration::from_millis(100));

        let gesture = sm.on_up(
            200.0,
            650.0,
            t0 + Duration::from_millis(120),
            viewport_height,
        );
        assert_eq!(gesture, Some(NavigationGesture::ReturnHome));
    }

    #[test]
    fn test_state_machine_overview_hold_gesture() {
        let mut sm = GestureStateMachine::default();
        let viewport_height = 915.0;

        // Start drag from bottom bar
        let t0 = Duration::from_millis(500);
        sm.on_down(200.0, 890.0, t0);

        // Drag up and hold for 500ms (> 400ms hold threshold)
        sm.on_move(200.0, 800.0, t0 + Duration::from_millis(100));
        sm.on_move(200.0, 720.0, t0 + Duration::from_millis(500));

        let gesture = sm.on_up(
            200.0,
            720.0,
            t0 + Duration::from_millis(550),
            viewport_height,
        );
        assert_eq!(gesture, Some(NavigationGesture::OverviewRecents));
    }

    #[test]
    fn test_state_machine_quick_settings_pull_down() {
        let mut sm = GestureStateMachine::default();
        let viewport_height = 915.0;

        // Pull down from status bar (y <= 60.0)
        let t0 = Duration::from_millis(100);
        sm.on_down(200.0, 20.0, t0);
        sm.on_move(200.0, 150.0, t0 + Duration::from_millis(100));

        let gesture = sm.on_up(
            200.0,
            250.0,
            t0 + Duration::from_millis(150),
            viewport_height,
        );
        assert_eq!(gesture, Some(NavigationGesture::OpenQuickSettings));
    }

    #[test]
    fn test_velocity_tracker_ema() {
        use crate::velocity::VelocityTracker;
        let mut tracker = VelocityTracker::new(0.8);
        let t0 = Duration::from_millis(0);
        tracker.start(100.0, 500.0, t0);

        // Move 100px up in 100ms -> inst_vy = -1000.0 px/s
        let (_, vy) = tracker.update(100.0, 400.0, t0 + Duration::from_millis(100));
        assert!((vy - (-1000.0)).abs() < 50.0);

        // Move another 100px up in 100ms -> inst_vy = -1000.0 px/s
        let (_, vy2) = tracker.update(100.0, 300.0, t0 + Duration::from_millis(200));
        assert!((vy2 - (-1000.0)).abs() < 50.0);
    }

    #[test]
    fn test_overview_spring_physics() {
        use crate::spring::SpringSolver;
        let mut spring = SpringSolver::overview()
            .with_position(0.0)
            .with_target(1.0)
            .with_velocity(-1.5);

        let dt = 1.0 / 60.0;
        for _ in 0..60 {
            spring.step(dt);
        }
        // At 1 second, it should have reached close to 1.0
        assert!((spring.position - 1.0).abs() < 0.05);
    }

    #[test]
    fn test_navigation_arbitration() {
        use crate::gesture_state::{
            arbitrate_horizontal_release, arbitrate_release, compute_normalized_progress,
            NavigationTarget,
        };
        let screen_h = 915.0;

        // Small upward drag (-50px) at low velocity -> Fullscreen
        let t = compute_normalized_progress(-50.0, screen_h);
        assert_eq!(arbitrate_release(t, -100.0), NavigationTarget::Fullscreen);

        // Fast upward flick -> Home (Authentic Android navigation)
        assert_eq!(arbitrate_release(t, -800.0), NavigationTarget::Home);

        // Fast downward fling -> Fullscreen
        assert_eq!(arbitrate_release(0.8, 700.0), NavigationTarget::Fullscreen);

        // Drag and pause past threshold (t >= 0.35) at low velocity -> Overview
        let t_large = compute_normalized_progress(-200.0, screen_h);
        assert_eq!(arbitrate_release(t_large, 0.0), NavigationTarget::Overview);

        // Horizontal Quick Switch arbitration:
        let screen_w = 412.0;
        // Fast swipe left -> NextApp
        assert_eq!(
            arbitrate_horizontal_release(-30.0, -600.0, screen_w),
            NavigationTarget::NextApp
        );
        // Fast swipe right -> PrevApp
        assert_eq!(
            arbitrate_horizontal_release(30.0, 600.0, screen_w),
            NavigationTarget::PrevApp
        );
        // Small horizontal drag without flick -> Fullscreen
        assert_eq!(
            arbitrate_horizontal_release(10.0, 50.0, screen_w),
            NavigationTarget::Fullscreen
        );
    }
}

