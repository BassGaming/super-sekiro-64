//! Near-side placement for carried/thrown bosses when a swept ray hits the map.

pub fn stop_before_hit(start: [f32; 3], hit: [f32; 3], radius: f32) -> [f32; 3] {
    let delta: [f32; 3] = std::array::from_fn(|i| hit[i] - start[i]);
    let distance = delta.iter().map(|v| v * v).sum::<f32>().sqrt();
    if distance < 0.0001 {
        return start;
    }
    // Do not push backward through other geometry when a wall is already closer
    // than the boss radius. Keep a small separation from the contact surface.
    let fraction = ((distance - radius.max(0.0) - 0.02) / distance).max(0.0);
    std::array::from_fn(|i| start[i] + delta[i] * fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carried_body_stays_before_a_wall() {
        let result = stop_before_hit([0.0; 3], [2.0, 0.0, 0.0], 0.5);
        assert!((result[0] - 1.48).abs() < 0.00001);
    }

    #[test]
    fn close_contact_does_not_teleport_backward() {
        assert_eq!(stop_before_hit([0.0; 3], [0.1, 0.0, 0.0], 0.5), [0.0; 3]);
        assert_eq!(stop_before_hit([1.0; 3], [1.0; 3], 0.5), [1.0; 3]);
    }

    #[test]
    fn ragdoll_crossing_is_rewound_to_the_near_side() {
        let result = stop_before_hit([10.0, 3.0, 0.0], [8.0, 3.0, 0.0], 0.75);
        assert!((result[0] - 8.77).abs() < 0.00001);
        assert_eq!(result[1], 3.0);
    }
}
