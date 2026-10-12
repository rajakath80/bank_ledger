import { useEffect, useState } from "react";
import { userManager } from "./oidc";

export default function App() {
  const [user, setUser] = useState<any>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    if (window.location.pathname === "/auth/callback") {
      userManager
        .signinCallback()
        .then((u) => {
          window.history.replaceState({}, "", "/");
          setUser(u);
        })
        .catch((e) => {
          console.error(e);
          setError(e.message);
        });
    }
  }, []);

  if (error) {
    return <pre>Login failed: {error}</pre>;
  }

  if (user) {
    return (
      <div>
        <h2>Login successful</h2>

        <h3>Access token</h3>
        <textarea
          readOnly
          value={user.access_token}
          style={{ width: "100%", height: 180 }}
        />

        <h3>ID token claims</h3>
        <pre>{JSON.stringify(user.profile, null, 2)}</pre>
      </div>
    );
  }

  return (
    <div>
      <h1>Kanidm OIDC Test</h1>

      <button onClick={() => userManager.signinRedirect()}>
        Login with Kanidm
      </button>
    </div>
  );
}