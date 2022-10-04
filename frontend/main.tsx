import {
  Fragment,
  h,
  JSX,
  render,
} from "https://esm.sh/preact@10.11.0/preact.js?target=deno";
import {
  useEffect,
  useState,
} from "https://esm.sh/preact@10.11.0/hooks.js?target=deno";
import {
  create,
  CredentialCreationOptionsJSON,
  CredentialRequestOptionsJSON,
  get,
  parseCreationOptionsFromJSON,
  parseRequestOptionsFromJSON,
} from "https://esm.sh/@github/webauthn-json@2.0.1/browser-ponyfill.js?target=deno";

import { domainCheck } from "./domaincheck.ts";

type Credential = {
  id: string;
  name: string;
};

async function checkIfAuthenticated(): Promise<boolean> {
  const response = await fetch("/api/validate", { method: "GET" });
  return response.ok;
}

async function startAuthentication(): Promise<
  CredentialRequestOptions | undefined
> {
  const response = await fetch("/api/authenticate", { method: "GET" });
  if (!response.ok) {
    throw new Error("Failed to start authentication");
  }
  const data: { challenge: null | CredentialRequestOptionsJSON } =
    await response.json();
  if (data.challenge === null) return undefined;
  return parseRequestOptionsFromJSON(data.challenge);
}

async function endAuthentication(opts: CredentialRequestOptions) {
  const data = await get(opts);
  const body = JSON.stringify(data);

  const response = await fetch("/api/authenticate", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body,
  });

  if (!response.ok) {
    throw new Error("Not authenticated");
  }
}

async function getCredentials(): Promise<Array<Credential>> {
  const response = await fetch("/api/credentials", { method: "GET" });
  const data: { data: Array<Credential> } = await response.json();
  return data.data;
}

function App() {
  const [loading, setLoading] = useState<boolean>(true);
  const [authenticated, setAuthenticated] = useState<boolean>(false);
  const [refresh, setRefresh] = useState<boolean>(true);
  const [newCredential, setNewCredential] = useState<string>("");
  const [credentials, setCredentials] = useState<Array<Credential>>([]);

  useEffect(() => {
    if (authenticated) return;

    if (!window.PublicKeyCredential) return;

    const params = new URLSearchParams(window.location.search);
    const redirect_url = params.get("url");

    function resolveAuth() {
      if (redirect_url !== null && domainCheck(document.domain, redirect_url)) {
        window.location.replace(redirect_url);
      } else {
        setAuthenticated(true);
        setLoading(false);
      }
    }

    setLoading(true);

    checkIfAuthenticated().then((isAuthenticated) => {
      if (isAuthenticated) {
        resolveAuth();
      } else {
        startAuthentication().then((data) => {
          if (data !== undefined) {
            // we have a challenge
            endAuthentication(data).then(() => {
              resolveAuth();
            }).catch(alert);
          }
        }).catch(alert);
      }
    });
  }, [authenticated]);

  useEffect(() => {
    if (!authenticated) return;
    if (!refresh) return;
    getCredentials().then(setCredentials);
    setRefresh(false);
  }, [authenticated, refresh]);

  const registerCredential = async function (
    e: JSX.TargetedEvent<HTMLFormElement, Event>,
  ) {
    e.preventDefault();
    if (newCredential === "") {
      alert("Name for new credential is empty");
      return;
    }
    try {
      const response = await fetch("/api/register", { method: "GET" });
      const startData: CredentialCreationOptionsJSON = await response.json();
      const endData = await create(parseCreationOptionsFromJSON(startData));
      const body = JSON.stringify({ name: newCredential, credential: endData });
      await fetch("/api/register", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
      });
      setRefresh(true);
      setNewCredential("");
    } catch (err) {
      console.error(err);
      alert(err);
    }
  };

  const deleteCredential = async function (cred_name: string) {
    try {
      await fetch(`/api/credentials/${cred_name}`, { method: "DELETE" });
      setRefresh(true);
    } catch (err) {
      console.error(err);
      alert(err);
    }
  };

  return (
    <>
      {loading ? <></> : (
        <>
          <h2>WebauthnTiny</h2>
          {authenticated
            ? (
              <>
                {window.PublicKeyCredential
                  ? (
                    <>
                      <div>
                        <h4>add a new credential</h4>
                        <form onSubmit={registerCredential}>
                          <label>
                            <input
                              type="text"
                              placeholder="name"
                              value={newCredential}
                              onInput={(e) =>
                                setNewCredential(
                                  (e.target as HTMLInputElement).value,
                                )}
                            />
                          </label>
                          <input type="submit" value={"\u{002b}"} />
                        </form>
                      </div>
                      <div>
                        <h4>
                          {credentials.length > 0
                            ? (
                              <>
                                existing credentials
                              </>
                            )
                            : (
                              <>
                                no existing credentials
                              </>
                            )}
                        </h4>
                        {credentials.map((cred) => (
                          <div key={cred.id}>
                            <button onClick={() => deleteCredential(cred.name)}>
                              {"\u{2212}"}
                            </button>
                            {cred.name}
                          </div>
                        ))}
                      </div>
                    </>
                  )
                  : <h4>this browser does not support webauthn</h4>}
              </>
            )
            : <h4>you are not authenticated</h4>}
        </>
      )}
    </>
  );
}

render(<App />, document.getElementById("app") as Element);
