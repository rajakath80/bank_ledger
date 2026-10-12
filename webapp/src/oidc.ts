import { UserManager, WebStorageStateStore } from "oidc-client-ts";

export const userManager = new UserManager({
  authority: "https://localhost:8443/oauth2/openid/ledger-web",
  client_id: "ledger-web",
  redirect_uri: "http://localhost:5173/auth/callback",
  response_type: "code",
  scope: "openid groups_name",

  userStore: new WebStorageStateStore({
    store: window.sessionStorage,
  }),
});