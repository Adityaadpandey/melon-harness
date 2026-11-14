import { execFileSync } from "node:child_process";

// The date a file last changed in git, as an ISO string, or undefined when git can't say
// (a shallow clone, a build with no .git). Returning nothing is deliberate: a made-up "modified today"
// on every page teaches Google to ignore the date, so we only send it when it is true.
export function lastModified(file: string): string | undefined {
  try {
    const out = execFileSync("git", ["log", "-1", "--format=%cI", "--", file], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
    return out || undefined;
  } catch {
    return undefined;
  }
}
