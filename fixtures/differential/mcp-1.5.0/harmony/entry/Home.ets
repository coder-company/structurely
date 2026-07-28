import { fetchProfile } from "profile-data"

export function loadProfile(): string {
  return fetchProfile();
}
