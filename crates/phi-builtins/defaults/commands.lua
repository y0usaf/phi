-- Phi's default session slash commands.
--
-- This is the same `phi.register_command` call a user extension would
-- make. Phi ships it as the default so the product works out of the box,
-- and so users can read it, copy it, and learn from it.
--
-- Every handler returns a table with `message` and/or `actions` — the
-- declarative UiAction vocabulary the frontend interprets. None of these
-- handlers touch UI state directly.

-- ── /login and /logout ──────────────────────────────────────────────────

local OAUTH_PROVIDERS = { "anthropic", "openai" }

local function trim(s)
    return (s:match("^%s*(.-)%s*$"))
end

phi.register_command("login", {
    description = "authenticate via OAuth (anthropic | openai)",
    args_hint = "[provider]",
    completions = OAUTH_PROVIDERS,
    handler = function(args)
        return {
            actions = {
                { type = "StartLogin", data = { provider = trim(args) } },
            },
        }
    end,
})

phi.register_command("logout", {
    description = "remove stored credentials",
    args_hint = "[provider]",
    completions = OAUTH_PROVIDERS,
    handler = function(args)
        return {
            actions = {
                { type = "Logout", data = { provider = trim(args) } },
            },
        }
    end,
})

-- ── /model and /submodel ────────────────────────────────────────────────
-- With an argument: switch to that model. Without: open the picker overlay.

local function model_command(name, description, root, completions)
    phi.register_command(name, {
        description = description,
        args_hint = "[model]",
        completions = completions,
        handler = function(args)
            local query = trim(args)
            if query == "" then
                return {
                    actions = {
                        { type = "OpenOverlay", data = { name = "phi:model", payload = { root = root } } },
                    },
                }
            end
            return {
                actions = {
                    { type = "SwitchModel", data = { root = root, query = query } },
                },
            }
        end,
    })
end

-- _phi_model_names is set by the Rust side (session_builder) before this
-- source executes; it's the list of provider/id strings for /model completion.
local model_completions = _phi_model_names or {}
model_command("model", "switch the root model (no arg: open the selector)", true, model_completions)
model_command("submodel", "switch the sub model used by llm_query", false, model_completions)

-- ── /resume, /tree, /fork — host-owned pickers ──────────────────────────

phi.register_command("resume", {
    description = "pick a past session for this directory to resume",
    handler = function()
        return { actions = { { type = "OpenPicker", data = "Resume" } } }
    end,
})

phi.register_command("tree", {
    description = "navigate the session tree (also esc esc)",
    handler = function()
        return { actions = { { type = "OpenPicker", data = "Tree" } } }
    end,
})

phi.register_command("fork", {
    description = "fork a new session from a prior user message",
    handler = function()
        return { actions = { { type = "OpenPicker", data = "Fork" } } }
    end,
})

-- ── /new, /usage, /help, /quit, /exit ───────────────────────────────────

phi.register_command("new", {
    description = "reset the session (fresh REPL, history cleared)",
    handler = function()
        return { actions = { { type = "NewSession" } } }
    end,
})

phi.register_command("usage", {
    description = "show usage totals",
    handler = function()
        return { actions = { { type = "OpenOverlay", data = { name = "phi:usage" } } } }
    end,
})

phi.register_command("help", {
    description = "show commands and keys",
    handler = function()
        return { actions = { { type = "OpenOverlay", data = { name = "phi:help" } } } }
    end,
})

phi.register_command("quit", {
    description = "exit (also ctrl-d on an empty line)",
    handler = function()
        return { actions = { { type = "Quit" } } }
    end,
})

phi.register_command("exit", {
    description = "alias for /quit",
    handler = function()
        return { actions = { { type = "Quit" } } }
    end,
})

-- ── /style ──────────────────────────────────────────────────────────────
-- With an argument: activate that style pack (persisted). Without: list
-- the registered styles. Completion is not registry-driven yet — style
-- packs register during the same runtime build as this command, so the
-- names aren't known here (the /model gap, WS3.2; revisit for WS10.4).

phi.register_command("style", {
    description = "switch the visual style pack (no arg: list styles)",
    args_hint = "[name]",
    handler = function(args)
        return { actions = { { type = "SwitchStyle", data = { name = trim(args) } } } }
    end,
})

-- ── /trust ──────────────────────────────────────────────────────────────

phi.register_command("trust", {
    description = "trust this project's .phi/ extensions (revoke: untrust)",
    args_hint = "[revoke]",
    completions = { "revoke" },
    handler = function(args)
        local trusted = not trim(args):lower():match("revoke")
        return { actions = { { type = "TrustProject", data = { trusted = trusted } } } }
    end,
})
