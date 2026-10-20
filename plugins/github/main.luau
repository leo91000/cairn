-- GitHub, entirely in Luau. Cairn signs the owner in, keeps the token, renders it into
-- granted agents' runs for git and the GitHub CLI, and calls these handlers for the
-- account's repositories, skill folders and CI.
local setup = require("./setup")
local projects = require("./projects")
local conditions = require("./conditions")
local skills = require("./skills")

return {
    -- Owner: the instance form, its test and its panel.
    form = setup.form,
    test = setup.test,
    view = setup.view,
    -- Repositories to import as projects.
    projects = projects.projects,
    -- What agents wait for with wait_for.
    condition = conditions.condition,
    -- Skills imported from repository folders.
    skills = skills.skills,
}
