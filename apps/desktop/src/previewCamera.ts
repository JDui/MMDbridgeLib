// A sphere must fit the narrower camera angle, including portrait viewports.
export function sphereCameraDistance(radius: number, fovDegrees: number, aspect: number): number {
  const verticalHalf = Math.max(1, Math.min(179, fovDegrees)) * Math.PI / 360;
  const horizontalHalf = Math.atan(Math.tan(verticalHalf) * Math.max(aspect, 0.01));
  return Math.max(radius, 0.001) / Math.sin(Math.min(verticalHalf, horizontalHalf)) * 1.08;
}
