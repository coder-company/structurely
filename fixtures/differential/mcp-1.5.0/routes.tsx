import { showUser as UserPage } from "./handlers";

export function AppRoutes() {
  return (
    <Routes>
      <Route path="/users/:id" element={<UserPage />} />
    </Routes>
  );
}
