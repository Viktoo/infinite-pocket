export const meta = { name: "pen", bounds: [3.2, 1.6, 3.2], tags: ["fence"] };
export function sdf(x, y, z, k) {
  // Three walls higher than a step, open towards -z.
  const back = box(x, y - 0.75, z - 3, 3.15, 0.75, 0.15);
  const left = box(x + 3, y - 0.75, z, 0.15, 0.75, 3);
  const right = box(x - 3, y - 0.75, z, 0.15, 0.75, 3);
  return min(back, min(left, right));
}
export function color(x, y, z, k) { return rgb(150, 120, 90); }
