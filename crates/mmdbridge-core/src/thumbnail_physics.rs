use glam::{Mat4, Quat, Vec3};
use mmd_anim_format::pmx::{PmxParsedJoint, PmxParsedMorph, PmxParsedRigidBody};
use rapier3d::prelude::nalgebra as na;
use rapier3d::prelude::*;

const PHYSICS_STEP_SECONDS: f32 = 1.0 / 65.0;
const PHYSICS_SUBSTEPS: u32 = 2;
const MAX_IMPULSE_MAGNITUDE: f32 = 10_000.0;

pub(crate) struct ImpulsePreviewResult {
    pub bone_world_matrices: Vec<Option<Mat4>>,
    pub applied_offsets: usize,
    pub simulated_substeps: u32,
}

struct BodyBinding {
    handle: Option<RigidBodyHandle>,
    bone_index: Option<usize>,
    body_local_to_bone: Option<Mat4>,
    mode: BodyMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyMode {
    FollowBone,
    Dynamic,
    DynamicBone,
    Unsupported,
}

pub(crate) fn simulate_impulse_preview(
    morphs: &[PmxParsedMorph],
    morph_weights: &[f32],
    rigid_body_specs: &[PmxParsedRigidBody],
    joint_specs: &[PmxParsedJoint],
    current_bone_world: &[Mat4],
    rest_bone_world: &[Mat4],
    diagnostics: &mut Vec<String>,
) -> Option<ImpulsePreviewResult> {
    let mut active_offsets = Vec::new();
    for (morph_index, morph) in morphs.iter().enumerate() {
        let weight = morph_weights.get(morph_index).copied().unwrap_or_default();
        if !weight.is_finite() || weight.abs() <= f32::EPSILON {
            continue;
        }
        for offset in &morph.impulse_offsets {
            active_offsets.push((morph_index, offset, weight));
        }
    }
    if active_offsets.is_empty() {
        return None;
    }

    let mut body_set = RigidBodySet::new();
    let mut collider_set = ColliderSet::new();
    let mut impulse_joints = ImpulseJointSet::new();
    let mut multibody_joints = MultibodyJointSet::new();
    let mut island_manager = IslandManager::new();
    let mut bindings = Vec::with_capacity(rigid_body_specs.len());
    let mut initial_poses = Vec::with_capacity(rigid_body_specs.len());
    let anchor = body_set.insert(RigidBodyBuilder::fixed().build());

    for (index, spec) in rigid_body_specs.iter().enumerate() {
        let mode = match spec.mode.as_str() {
            "static" => BodyMode::FollowBone,
            "dynamic" => BodyMode::Dynamic,
            "dynamicBone" => BodyMode::DynamicBone,
            _ => BodyMode::Unsupported,
        };
        let bone_index = usize::try_from(spec.bone_index)
            .ok()
            .filter(|bone_index| *bone_index < current_bone_world.len());
        let body_rest = pmx_transform(spec.position, spec.rotation);
        let body_local_to_bone = bone_index
            .and_then(|bone_index| rest_bone_world.get(bone_index).copied())
            .filter(|matrix| matrix.is_finite())
            .map(|bone_rest| bone_rest.inverse() * body_rest)
            .filter(|matrix| matrix.is_finite());
        let current_body = bone_index
            .and_then(|bone_index| current_bone_world.get(bone_index).copied())
            .zip(body_local_to_bone)
            .map(|(bone, local)| bone * local)
            .unwrap_or(body_rest);
        let initial_pose = mat4_to_isometry(current_body);
        if initial_pose.is_none() {
            diagnostics.push(format!("InvalidRigidBodyTransform:{index}"));
        }

        let position_damping = finite_unit(spec.linear_damping);
        let rotation_damping = finite_unit(spec.angular_damping);
        let mass = if spec.mass.is_finite() && spec.mass > 0.0 {
            spec.mass
        } else {
            0.001
        };
        let shape = collider_shape(spec);
        let builder = match mode {
            BodyMode::FollowBone => RigidBodyBuilder::kinematic_position_based(),
            BodyMode::Dynamic | BodyMode::DynamicBone if shape.is_some() => {
                RigidBodyBuilder::dynamic()
            }
            BodyMode::Dynamic | BodyMode::DynamicBone => {
                RigidBodyBuilder::dynamic().additional_mass(mass)
            }
            BodyMode::Unsupported => RigidBodyBuilder::fixed(),
        };
        let handle = initial_pose.map(|pose| {
            body_set.insert(
                builder
                    .position(pose)
                    .linear_damping(position_damping)
                    .angular_damping(rotation_damping)
                    .sleeping(false)
                    .build(),
            )
        });
        if let Some(handle) = handle {
            if let Some(shape) = shape {
                let group = if spec.group < 16 {
                    1u32 << spec.group
                } else {
                    diagnostics.push(format!("InvalidRigidBodyGroup:{index}:{}", spec.group));
                    1
                };
                let collide_with = u32::from(!spec.mask);
                let collider = shape
                    .friction(finite_unit(spec.friction))
                    .restitution(finite_unit(spec.restitution))
                    .mass(mass)
                    .collision_groups(InteractionGroups::new(
                        Group::from_bits_truncate(group),
                        Group::from_bits_truncate(collide_with),
                    ))
                    .build();
                collider_set.insert_with_parent(collider, handle, &mut body_set);
            } else {
                diagnostics.push(format!("UnsupportedRigidBodyShape:{index}:{}", spec.shape));
            }
        }
        if mode == BodyMode::Unsupported {
            diagnostics.push(format!("UnsupportedRigidBodyMode:{index}:{}", spec.mode));
        }
        bindings.push(BodyBinding {
            handle,
            bone_index,
            body_local_to_bone,
            mode,
        });
        initial_poses.push(initial_pose);
    }

    for (index, spec) in joint_specs.iter().enumerate() {
        let body_a = joint_body_handle(spec.rigid_body_index_a, &bindings, anchor);
        let body_b = joint_body_handle(spec.rigid_body_index_b, &bindings, anchor);
        let (Some(body_a), Some(body_b)) = (body_a, body_b) else {
            diagnostics.push(format!("InvalidPhysicsJointBody:{index}"));
            continue;
        };
        if body_a == body_b {
            diagnostics.push(format!("DegeneratePhysicsJoint:{index}"));
            continue;
        }
        let Some(joint_world) = pmx_isometry(spec.position, spec.rotation) else {
            diagnostics.push(format!("InvalidPhysicsJointTransform:{index}"));
            continue;
        };
        let pose_a = pose_for_joint_body(spec.rigid_body_index_a, &initial_poses);
        let pose_b = pose_for_joint_body(spec.rigid_body_index_b, &initial_poses);
        let (Some(pose_a), Some(pose_b)) = (pose_a, pose_b) else {
            diagnostics.push(format!("InvalidPhysicsJointTransform:{index}"));
            continue;
        };
        let Some(mut joint) = build_joint(spec, joint_world, pose_a, pose_b) else {
            diagnostics.push(format!("UnsupportedPhysicsJoint:{index}:{}", spec.kind));
            continue;
        };
        joint.set_contacts_enabled(false);
        impulse_joints.insert(body_a, body_b, joint, true);
    }

    let mut applied_offsets = 0usize;
    for (morph_index, offset, weight) in active_offsets {
        let Ok(body_index) = usize::try_from(offset.rigid_body_index) else {
            diagnostics.push(format!(
                "InvalidImpulseRigidBody:{morph_index}:{}",
                offset.rigid_body_index
            ));
            continue;
        };
        let Some(binding) = bindings.get(body_index) else {
            diagnostics.push(format!(
                "InvalidImpulseRigidBody:{morph_index}:{}",
                offset.rigid_body_index
            ));
            continue;
        };
        if !matches!(binding.mode, BodyMode::Dynamic | BodyMode::DynamicBone) {
            diagnostics.push(format!(
                "ImpulseTargetNotDynamic:{morph_index}:{body_index}"
            ));
            continue;
        }
        let Some(handle) = binding.handle else {
            diagnostics.push(format!(
                "ImpulseTargetUnavailable:{morph_index}:{body_index}"
            ));
            continue;
        };
        let mut velocity = Vec3::from_array(offset.velocity) * weight;
        let mut torque = Vec3::from_array(offset.torque) * weight;
        if !velocity.is_finite() || !torque.is_finite() {
            diagnostics.push(format!("InvalidImpulseOffset:{morph_index}:{body_index}"));
            continue;
        }
        if velocity.abs().max_element() > MAX_IMPULSE_MAGNITUDE
            || torque.abs().max_element() > MAX_IMPULSE_MAGNITUDE
        {
            diagnostics.push(format!(
                "ImpulseMagnitudeOutOfRange:{morph_index}:{body_index}"
            ));
            continue;
        }
        let Some(body) = body_set.get_mut(handle) else {
            continue;
        };
        if offset.local {
            let local_velocity = body.rotation() * vector![velocity.x, velocity.y, velocity.z];
            let local_torque = body.rotation() * vector![torque.x, torque.y, torque.z];
            velocity = Vec3::new(local_velocity.x, local_velocity.y, local_velocity.z);
            torque = Vec3::new(local_torque.x, local_torque.y, local_torque.z);
        }
        body.apply_impulse(vector![velocity.x, velocity.y, velocity.z], true);
        body.apply_torque_impulse(vector![torque.x, torque.y, torque.z], true);
        applied_offsets += 1;
    }

    if applied_offsets == 0 {
        return Some(ImpulsePreviewResult {
            bone_world_matrices: vec![None; current_bone_world.len()],
            applied_offsets,
            simulated_substeps: 0,
        });
    }

    let mut physics_pipeline = PhysicsPipeline::new();
    let mut broad_phase = BroadPhase::new();
    let mut narrow_phase = NarrowPhase::new();
    let mut ccd_solver = CCDSolver::new();
    let integration = IntegrationParameters {
        dt: PHYSICS_STEP_SECONDS,
        ..IntegrationParameters::default()
    };
    let gravity = vector![0.0, -98.0, 0.0];
    for _ in 0..PHYSICS_SUBSTEPS {
        physics_pipeline.step(
            &gravity,
            &integration,
            &mut island_manager,
            &mut broad_phase,
            &mut narrow_phase,
            &mut body_set,
            &mut collider_set,
            &mut impulse_joints,
            &mut multibody_joints,
            &mut ccd_solver,
            None,
            &(),
            &(),
        );
    }

    let mut bone_world_matrices = vec![None; current_bone_world.len()];
    let mut bone_priorities = vec![0u8; current_bone_world.len()];
    for (body_index, binding) in bindings.iter().enumerate() {
        let (Some(bone_index), Some(local), Some(handle)) = (
            binding.bone_index,
            binding.body_local_to_bone,
            binding.handle,
        ) else {
            continue;
        };
        if !matches!(binding.mode, BodyMode::Dynamic | BodyMode::DynamicBone) {
            continue;
        }
        let Some(body) = body_set.get(handle) else {
            continue;
        };
        let body_world = isometry_to_mat4(body.position());
        let mut bone_world = body_world * local.inverse();
        if binding.mode == BodyMode::DynamicBone {
            let (_, rotation, _) = bone_world.to_scale_rotation_translation();
            let (scale, _, translation) =
                current_bone_world[bone_index].to_scale_rotation_translation();
            bone_world = Mat4::from_scale_rotation_translation(scale, rotation, translation);
        }
        if !bone_world.is_finite() {
            diagnostics.push(format!(
                "InvalidPhysicsBoneTransform:{body_index}:{bone_index}"
            ));
            continue;
        }
        let priority = if binding.mode == BodyMode::Dynamic {
            2
        } else {
            1
        };
        if bone_world_matrices[bone_index].is_some() {
            diagnostics.push(format!("MultiplePhysicsBodiesForBone:{bone_index}"));
        }
        if priority > bone_priorities[bone_index] {
            bone_world_matrices[bone_index] = Some(bone_world);
            bone_priorities[bone_index] = priority;
        }
    }

    Some(ImpulsePreviewResult {
        bone_world_matrices,
        applied_offsets,
        simulated_substeps: PHYSICS_SUBSTEPS,
    })
}

fn collider_shape(spec: &PmxParsedRigidBody) -> Option<ColliderBuilder> {
    match spec.shape.as_str() {
        "sphere" if spec.size[0].is_finite() && spec.size[0] > 0.0 => {
            Some(ColliderBuilder::ball(spec.size[0]))
        }
        "box" if spec.size.iter().all(|size| size.is_finite() && *size > 0.0) => Some(
            ColliderBuilder::cuboid(spec.size[0], spec.size[1], spec.size[2]),
        ),
        "capsule"
            if spec.size[0].is_finite()
                && spec.size[0] > 0.0
                && spec.size[1].is_finite()
                && spec.size[1] >= 0.0 =>
        {
            Some(ColliderBuilder::capsule_y(spec.size[1] * 0.5, spec.size[0]))
        }
        _ => None,
    }
}

fn joint_body_handle(
    index: i32,
    bindings: &[BodyBinding],
    anchor: RigidBodyHandle,
) -> Option<RigidBodyHandle> {
    if index == -1 {
        return Some(anchor);
    }
    usize::try_from(index)
        .ok()
        .and_then(|index| bindings.get(index))
        .and_then(|binding| binding.handle)
}

fn pose_for_joint_body(
    index: i32,
    initial_poses: &[Option<Isometry<f32>>],
) -> Option<Isometry<f32>> {
    if index == -1 {
        return Some(Isometry::identity());
    }
    usize::try_from(index)
        .ok()
        .and_then(|index| initial_poses.get(index).copied().flatten())
}

fn build_joint(
    spec: &PmxParsedJoint,
    joint_world: Isometry<f32>,
    body_a_world: Isometry<f32>,
    body_b_world: Isometry<f32>,
) -> Option<GenericJoint> {
    let axes = [
        JointAxis::X,
        JointAxis::Y,
        JointAxis::Z,
        JointAxis::AngX,
        JointAxis::AngY,
        JointAxis::AngZ,
    ];
    let lower = [
        spec.translation_lower_limit[0],
        spec.translation_lower_limit[1],
        spec.translation_lower_limit[2],
        spec.rotation_lower_limit[0],
        spec.rotation_lower_limit[1],
        spec.rotation_lower_limit[2],
    ];
    let upper = [
        spec.translation_upper_limit[0],
        spec.translation_upper_limit[1],
        spec.translation_upper_limit[2],
        spec.rotation_upper_limit[0],
        spec.rotation_upper_limit[1],
        spec.rotation_upper_limit[2],
    ];
    let spring = [
        spec.spring_translation_factor[0],
        spec.spring_translation_factor[1],
        spec.spring_translation_factor[2],
        spec.spring_rotation_factor[0],
        spec.spring_rotation_factor[1],
        spec.spring_rotation_factor[2],
    ];
    let mut builder = GenericJointBuilder::new(JointAxesMask::empty())
        .local_frame1(body_a_world.inverse() * joint_world)
        .local_frame2(body_b_world.inverse() * joint_world);
    let mut locked_axes = JointAxesMask::empty();
    for (index, axis) in axes.into_iter().enumerate() {
        let policy = joint_axis_policy(&spec.kind, index)?;
        match policy {
            AxisPolicy::Free => {}
            AxisPolicy::Locked => locked_axes |= JointAxesMask::from(axis),
            AxisPolicy::Limited => {
                if !lower[index].is_finite()
                    || !upper[index].is_finite()
                    || lower[index] > upper[index]
                {
                    locked_axes |= JointAxesMask::from(axis);
                } else {
                    builder = builder.limits(axis, [lower[index], upper[index]]);
                }
            }
        }
        if spec.kind == "generic6dofSpring" && spring[index].is_finite() && spring[index] > 0.0 {
            let stiffness = spring[index].min(100_000.0);
            builder = builder
                .motor_model(axis, MotorModel::ForceBased)
                .motor_position(axis, 0.0, stiffness, stiffness.sqrt() * 0.2);
        }
    }
    Some(builder.locked_axes(locked_axes).build())
}

#[derive(Clone, Copy)]
enum AxisPolicy {
    Free,
    Locked,
    Limited,
}

fn joint_axis_policy(kind: &str, axis: usize) -> Option<AxisPolicy> {
    Some(match kind {
        "generic6dofSpring" | "generic6dof" => AxisPolicy::Limited,
        "point2point" => {
            if axis < 3 {
                AxisPolicy::Locked
            } else {
                AxisPolicy::Free
            }
        }
        "coneTwist" => {
            if axis < 3 {
                AxisPolicy::Locked
            } else {
                AxisPolicy::Limited
            }
        }
        "slider" => match axis {
            0 => AxisPolicy::Limited,
            _ => AxisPolicy::Locked,
        },
        "hinge" => match axis {
            3 => AxisPolicy::Limited,
            _ => AxisPolicy::Locked,
        },
        _ => return None,
    })
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn pmx_transform(position: [f32; 3], rotation: [f32; 3]) -> Mat4 {
    let position = Vec3::from_array(position);
    let rotation = Quat::from_euler(glam::EulerRot::XYZ, rotation[0], rotation[1], rotation[2]);
    if !position.is_finite() || !rotation.is_finite() {
        Mat4::IDENTITY
    } else {
        Mat4::from_rotation_translation(rotation.normalize(), position)
    }
}

fn pmx_isometry(position: [f32; 3], rotation: [f32; 3]) -> Option<Isometry<f32>> {
    mat4_to_isometry(pmx_transform(position, rotation))
}

fn mat4_to_isometry(matrix: Mat4) -> Option<Isometry<f32>> {
    if !matrix.is_finite() {
        return None;
    }
    let (_, rotation, translation) = matrix.to_scale_rotation_translation();
    if !rotation.is_finite()
        || !translation.is_finite()
        || rotation.length_squared() <= f32::EPSILON
    {
        return None;
    }
    let rotation = rotation.normalize();
    Some(Isometry::from_parts(
        na::Translation3::new(translation.x, translation.y, translation.z),
        na::UnitQuaternion::from_quaternion(na::Quaternion::new(
            rotation.w, rotation.x, rotation.y, rotation.z,
        )),
    ))
}

fn isometry_to_mat4(isometry: &Isometry<f32>) -> Mat4 {
    let translation = isometry.translation.vector;
    let rotation = isometry.rotation.quaternion();
    Mat4::from_rotation_translation(
        Quat::from_xyzw(rotation.i, rotation.j, rotation.k, rotation.w),
        Vec3::new(translation.x, translation.y, translation.z),
    )
}
