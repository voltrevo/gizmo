#!/usr/bin/env bun

import { mkdirSync, readFileSync, writeFileSync, existsSync, readdirSync } from "fs";
import { join } from "path";
import { homedir } from "os";
import { GizmoClient } from "./client.ts";
import { generateKeypair, Signer } from "./crypto.ts";

const BASE_DIR = join(homedir(), ".local", "share", "gizmo");
const USERS_DIR = join(BASE_DIR, "users");

const args = process.argv.slice(2);
const command = args[0];

function flag(name: string): string | undefined {
  const i = args.indexOf(`--${name}`);
  if (i === -1 || i + 1 >= args.length) return undefined;
  return args[i + 1];
}

function hasFlag(name: string): boolean {
  return args.includes(`--${name}`);
}

function resolveUser(): string {
  return flag("user") ?? readStored("active-user", BASE_DIR) ?? "default";
}

function userDir(user?: string): string {
  return join(USERS_DIR, user ?? resolveUser());
}

function readStored(filename: string, dir?: string): string | undefined {
  const p = join(dir ?? userDir(), filename);
  if (!existsSync(p)) return undefined;
  return readFileSync(p, "utf-8").trim() || undefined;
}

function writeStored(filename: string, value: string, dir?: string): void {
  const d = dir ?? userDir();
  mkdirSync(d, { recursive: true });
  writeFileSync(join(d, filename), value + "\n");
}

function ensureKeypair(user?: string): { secretKey: string; publicKey: string } {
  const dir = userDir(user);
  const existing = readStored("secret-key", dir);
  if (existing) {
    const signer = new Signer(existing);
    return { secretKey: existing, publicKey: signer.publicKey };
  }
  const kp = generateKeypair();
  writeStored("secret-key", kp.secretKey, dir);
  console.error(`generated keypair for "${user ?? resolveUser()}" -> ${dir}`);
  return kp;
}

function require(value: string | undefined, name: string): string {
  if (!value) {
    console.error(`Missing: --${name}, ${name.toUpperCase().replace(/-/g, "_")} env var, or stored value`);
    process.exit(1);
  }
  return value;
}

function makeClient(): GizmoClient {
  const url = require(flag("url") ?? process.env.GIZMO_URL ?? readStored("url", BASE_DIR), "url");
  const token = require(flag("token") ?? process.env.GIZMO_TOKEN ?? readStored("token", BASE_DIR), "token");
  const secretKey = flag("secret-key") ?? process.env.GIZMO_SECRET_KEY ?? readStored("secret-key");
  return new GizmoClient({ url, token, signer: new Signer(require(secretKey, "secret-key")) });
}

async function main() {
  switch (command) {
    case "keygen": {
      const user = resolveUser();
      const kp = ensureKeypair(user);
      // Always regenerate if explicitly asked
      if (readStored("secret-key", userDir(user))) {
        // Already exists — only regenerate if --force
        if (hasFlag("force")) {
          const fresh = generateKeypair();
          writeStored("secret-key", fresh.secretKey, userDir(user));
          console.log(`secret_key: ${fresh.secretKey}`);
          console.log(`public_key: ${fresh.publicKey}`);
          console.error(`regenerated keypair for "${user}"`);
        } else {
          console.log(`secret_key: ${kp.secretKey}`);
          console.log(`public_key: ${kp.publicKey}`);
          console.error(`existing keypair for "${user}" (use --force to regenerate)`);
        }
      } else {
        console.log(`secret_key: ${kp.secretKey}`);
        console.log(`public_key: ${kp.publicKey}`);
      }
      writeStored("active-user", user, BASE_DIR);
      break;
    }

    case "users": {
      const active = resolveUser();
      if (!existsSync(USERS_DIR)) {
        console.log("(no users)");
        break;
      }
      for (const name of readdirSync(USERS_DIR).sort()) {
        const sk = readStored("secret-key", join(USERS_DIR, name));
        const pk = sk ? new Signer(sk).publicKey : "???";
        const marker = name === active ? " *" : "";
        console.log(`${name}${marker}  ${pk}`);
      }
      break;
    }

    case "config": {
      const url = flag("url");
      const token = flag("token");
      const secretKey = flag("secret-key");
      const user = flag("user");
      if (url) writeStored("url", url, BASE_DIR);
      if (token) writeStored("token", token, BASE_DIR);
      if (secretKey) writeStored("secret-key", secretKey);
      if (user) writeStored("active-user", user, BASE_DIR);
      if (url || token || secretKey || user) {
        if (user) ensureKeypair(user);
        console.error(`saved to ${BASE_DIR}`);
      } else {
        const u = resolveUser();
        console.log(`user:       ${u}`);
        console.log(`url:        ${readStored("url", BASE_DIR) ?? "(not set)"}`);
        console.log(`token:      ${readStored("token", BASE_DIR) ?? "(not set)"}`);
        const sk = readStored("secret-key");
        console.log(`secret-key: ${sk ?? "(not set)"}`);
        if (sk) console.log(`public-key: ${new Signer(sk).publicKey}`);
      }
      break;
    }

    case "publish": {
      ensureKeypair();
      const client = makeClient();
      const tags = flag("tags")?.split(",").map((s) => s.trim()) ?? [];
      const bodyRaw = flag("body");
      if (!bodyRaw) {
        console.error("--body is required");
        process.exit(1);
      }
      const body = JSON.parse(bodyRaw);
      const channel = flag("channel");
      const allow = flag("allow")
        ?.split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      const disallow = flag("disallow")
        ?.split(",")
        .map((s) => s.trim())
        .filter(Boolean);

      await client.connect();
      const id = await client.publish({ tags, body, channel, allow, disallow });
      console.log(`published: ${id}`);
      client.disconnect();
      break;
    }

    case "subscribe": {
      ensureKeypair();
      const client = makeClient();
      const tags = flag("tags")
        ?.split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      const channel = flag("channel");

      await client.connect();
      await client.subscribe(
        (msg) => {
          console.log(JSON.stringify(msg));
        },
        { tags, channel },
      );
      console.error(`subscribed as "${resolveUser()}", listening...`);
      break;
    }

    case "history": {
      const client = makeClient();
      const channel = flag("channel");
      const after = flag("after") ? Number(flag("after")) : undefined;
      const before = flag("before") ? Number(flag("before")) : undefined;
      const limit = flag("limit") ? Number(flag("limit")) : undefined;
      const tags = flag("tags")
        ?.split(",")
        .map((s) => s.trim())
        .filter(Boolean);

      const result = await client.history({
        channel,
        after,
        before,
        limit,
        tags,
      });
      console.log(JSON.stringify(result, null, 2));
      break;
    }

    default:
      console.log(`usage: gizmo <command> [options]

commands:
  keygen                Generate keypair for user (auto-generates on first use)
  users                 List all users and their public keys
  config                Show or set stored config values
  publish               Publish a message
  subscribe             Subscribe to live messages
  history               Fetch message history

identity:
  --user <name>         Use this identity (default: "default")
                        Auto-generates keypair if user doesn't exist yet
  keygen --force        Regenerate keypair for current user
  users                 List all users (* = active)

config:
  Reads from: --flag > ENV var > ~/.local/share/gizmo/
  config                Show current stored values
  config --url <url>    Store server URL
  config --token <t>    Store bearer token
  config --user <name>  Switch active user

common options (or set env vars):
  --url <url>           Server URL (GIZMO_URL)
  --token <token>       Bearer token (GIZMO_TOKEN)
  --secret-key <key>    Ed25519 secret key hex (GIZMO_SECRET_KEY)

publish options:
  --tags <a,b>          Comma-separated tags
  --body <json>         Message body (JSON)
  --channel <name>      Channel (default: "default")
  --allow <keys>        Comma-separated allowed public keys
  --disallow <keys>     Comma-separated disallowed public keys

subscribe options:
  --tags <a,b>          Filter by tags
  --channel <name>      Channel to subscribe to

history options:
  --channel <name>      Channel
  --after <id>          Messages after this ID
  --before <id>         Messages before this ID
  --limit <n>           Page size
  --tags <a,b>          Filter by tags`);
      if (command) process.exit(1);
  }
}

main().catch((e) => {
  console.error(e.message ?? e);
  process.exit(1);
});
