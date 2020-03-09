//! An asymmetric frustum with an arbitrary 3D pose.

use crate::math::{Cuboid, Isometry3, PointCulling};
use cgmath::{BaseFloat, Decomposed, Matrix4, Perspective, Point3, Quaternion, Transform, Vector3};
use collision::{Aabb3, Relation};
use serde::{Deserialize, Serialize};

/// A frustum is defined in eye coordinates, where x points right, y points up,
/// and z points against the viewing direction. This is not how e.g. OpenCV
/// defines a camera coordinate system. To get from OpenCV camera coordinates
/// to eye coordinates, you need to rotate 180 deg around the x axis before
/// creating the perspective projection, see also the frustum unit test below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frustum<S: BaseFloat> {
    world_from_clip: Matrix4<S>,
    frustum: collision::Frustum<S>,
}

impl<S: BaseFloat> Frustum<S> {
    pub fn new(world_from_eye: Isometry3<S>, clip_from_eye: Perspective<S>) -> Self {
        let eye_from_world: Decomposed<Vector3<S>, Quaternion<S>> = world_from_eye.inverse().into();
        let clip_from_world =
            Matrix4::<S>::from(clip_from_eye) * Matrix4::<S>::from(eye_from_world);
        let world_from_clip = clip_from_world.inverse_transform().unwrap();
        let frustum = collision::Frustum::from_matrix4(clip_from_world).unwrap();
        Frustum {
            world_from_clip,
            frustum,
        }
    }
}

impl<S> PointCulling<S> for Frustum<S>
where
    S: 'static + BaseFloat + Sync + Send,
{
    fn contains(&self, point: &Point3<S>) -> bool {
        match self.frustum.contains(point) {
            Relation::Cross => true,
            Relation::In => true,
            Relation::Out => false,
        }
    }
    fn intersects_aabb3(&self, aabb: &Aabb3<S>) -> bool {
        match self.frustum.contains(aabb) {
            Relation::Cross => true,
            Relation::In => true,
            Relation::Out => false,
        }
    }
}

impl<S> Cuboid<S> for Frustum<S>
where
    S: BaseFloat,
{
    fn corners(&self) -> [Point3<S>; 8] {
        let corner_from = |x, y, z| self.world_from_clip.transform_point(Point3::new(x, y, z));
        [
            corner_from(-S::one(), -S::one(), -S::one()),
            corner_from(-S::one(), -S::one(), S::one()),
            corner_from(-S::one(), S::one(), -S::one()),
            corner_from(-S::one(), S::one(), S::one()),
            corner_from(S::one(), -S::one(), -S::one()),
            corner_from(S::one(), -S::one(), S::one()),
            corner_from(S::one(), S::one(), -S::one()),
            corner_from(S::one(), S::one(), S::one()),
        ]
    }
}
