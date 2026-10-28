> Statut : précisée par [ADR-0075](0075-plugin-hooks-rules-and-scoped-grants.md) : la limite d’une instance par package ne vise plus que les identifiants de run, reçus seuls ou par un accès complet ; les autres contributions d’un même package peuvent venir de plusieurs instances.

# GitHub devient un plugin livré avec Cairn

Date : 2026-10-20.

## Constat

GitHub était câblé dans l'installation à six endroits : la connexion partagée (le
login `gh` du serveur, obtenu par `gh auth login`), le jeton dédié de chaque agent
(stocké en clair dans `kv`), le droit `access.github` des agents, l'import de
projets depuis un dépôt, les conditions `wait_for` (`github_workflow`,
`github_pr_checks`) et l'import de skills depuis un dépôt. Tout passait par le
binaire `gh` du serveur. Aucun autre service ne pouvait offrir ces fonctions, et un
package externe ne pouvait pas faire ce que faisait GitHub.

## Décision

GitHub est le package livré `plugins/github/`, écrit en Luau comme Discord et
1Password. L'installation ne contient plus de code GitHub ; elle offre des API de
plugin génériques, que n'importe quel package peut déclarer :

- **Connexion par appareil** (`credential.deviceFlow`) : l'hôte déroule le flux
  OAuth 2.0 « device authorization » (RFC 8628) pour le propriétaire, montre le code
  et garde le jeton en mémoire jusqu'à l'enregistrement de l'instance
  (`credentialFlow`). Ni le navigateur ni le script ne voient le jeton. Coller un
  jeton reste possible. GitHub utilise l'OAuth App « Cairn installations » de
  leo91000 (client ID public, device flow activé, jetons sans expiration).
- **Identifiants de run** (`runCredentials`) : fichiers du home (gabarits
  `{{credential}}`, `{{secrets.X}}`, `{{settings.X}}`), identifiants Git par préfixe
  HTTPS dans un fichier `credential-store` détenu par Cairn
  (`~/.config/cairn/git-credentials`), identité de commit. L'hôte les écrit pour les
  agents qui détiennent une instance, à chaque préparation et reprise de run ; un
  droit retiré vide les fichiers qu'il avait écrits, et la fin du run les supprime.
  Les mêmes identifiants servent aux récupérations Git de l'hôte : d'abord
  l'instance qui a importé le projet, puis celle de l'agent, puis toute instance qui
  sert le remote. Un remote SSH passe en HTTPS seulement si une instance le sert.
  Un agent détient au plus une instance par package qui déclare des identifiants de
  run.
- **Motifs de secrets** (`secretPatterns`) : formats de jetons masqués dans la
  sortie des runs.
- **Sources de projets** (`projectSources`, gestionnaire `projects`) : le package
  liste et résout ; Cairn clone avec les identifiants de l'instance et enregistre le
  projet avec sa source (`{ pluginId, source, instance, id }`).
- **Conditions** (`conditions`, gestionnaire `condition`) : leur type est
  `<plugin>_<nom>`, donc `github_workflow` et `github_pr_checks` gardent leur nom.
  Cairn garde l'attente durable, vérifie le droit et le projet, planifie les
  observations et reprend la conversation ; le package dit seulement si la condition
  est remplie, et fournit les textes affichés (`display`). Le portage Luau des
  checks requis reprend la logique Rust à l'identique.
- **Sources de skills** (`skillSources`, gestionnaire `skills`) : le package résout
  un emplacement en version et liste les fichiers d'un dossier ; Cairn télécharge
  depuis les hôtes déclarés avec l'identifiant de l'instance, choisit le dossier et
  compare les versions.

`access.github` disparaît : un compte GitHub est une instance de plugin accordée sur
la page de l'agent (ADR 0051). La règle « la connexion partagée exige tous les
projets » disparaît aussi : le propriétaire choisit le compte ou le jeton (un jeton
fin limite les dépôts). La règle « un agent ouvert à tout le monde n'a pas GitHub »
est couverte par celle qui interdit tout droit de plugin à cet agent.

La connexion « Se connecter avec GitHub » de Beacon reste : c'est un moyen de se
connecter à un compte Cairn, sans rapport avec l'installation.

## Conséquences

- Retirer un droit GitHub n'interrompt plus le travail en file (les droits de
  plugin sont vivants) ; une attente GitHub en cours se termine `unavailable` au
  lieu de mettre la conversation en pause.
- Les clients web et Android n'ont plus d'écran GitHub : la connexion se fait sur
  la page de l'instance du plugin, l'import de projets et de skills passe par les
  sources des plugins, les attentes affichent les textes du package.
- `gh` reste un outil fourni aux runs (image des VM, nodes natives) ; le serveur ne
  l'utilise plus. Le package écrit son `hosts.yml` pour les agents autorisés.
- Les exécutions partagées sans VM (développement local sans `RUNNER_URL`) ne
  reçoivent pas d'identifiants de run : elles utilisent l'environnement du serveur.
