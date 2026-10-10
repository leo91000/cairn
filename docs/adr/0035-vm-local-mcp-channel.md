# Canal MCP local à la VM, relayé par vsock

Date : 2026-10-10.

## Constat

L’agent s’exécute dans une VM. Il recevait `{PUBLIC_URL}/mcp-workspace` et
`{PUBLIC_URL}/mcp-gateway/{id}`. Sans URL publique (ADR-0028), cette origine est
privée (`http://manager:4310`) ou loopback. Depuis la VM, elle est injoignable :
le DNS de la VM ne connaît pas le réseau Compose, son pare-feu rejette les plages
privées et `127/8` de l’hôte, et aucun relais n’existait. Les agents perdaient
`cairn_workspace` et les connexions MCP (#162).

## Décision

Dans une VM, l’agent joint le MCP de son exécution à `http://127.0.0.1:5202`.

- Le superviseur invité (root, au démarrage) écoute ce port loopback et relaie
  chaque connexion par vsock vers le port 5202 de l’hôte, comme le socket
  d’authentification utilise déjà le port 5201.
- Pendant une tentative, le contrôleur relaie ce port vers un socket Unix privé
  du home de l’exécution, `cairn-mcp.sock`. Il ne connaît ni origine, ni DNS.
- Sur l’installation, le manager sert ce socket pendant la tentative. Il n’expose
  que `/mcp-workspace` et `/mcp-gateway/{id}`, et refuse un jeton d’une autre
  exécution.
- Sur un node distant, son connecteur sert ce socket. Il transmet chaque requête
  MCP au manager par sa session authentifiée existante
  (`/internal/node-workspace/{attempt}/mcp`). Le manager vérifie que la tentative
  appartient à ce node et que le jeton appartient à son exécution.

Les jetons restent liés à l’exécution et vérifiés par le manager. Le pare-feu
des VM est inchangé, aucun port n’est publié, et le manager n’est pas exposé
hors de son réseau privé.

## Options écartées

- **Ouvrir le pare-feu et le DNS vers l’origine du manager** : la VM aurait
  joint le réseau privé de l’installation, et un node distant n’a pas de route
  vers ce réseau.
- **Relayer au niveau TCP vers `PUBLIC_URL`** : il faut alors un chemin distinct
  pour les nodes distants, et le contrôleur dépend d’une origine résolue.
- **Écouteur dans `runner-entry`** (code courant même sur une image ancienne) :
  il n’existe que pendant une conversation, n’est pas root, et laisse sans canal
  les plans à commande. Le produit n’a pas encore de release : une installation
  neuve n’a pas de conversation sur une image antérieure.

## Conséquences

- Une conversation dont le disque reste sur une image antérieure à ce changement
  n’a pas d’écouteur `127.0.0.1:5202`, donc pas de MCP.
- Une exécution hors VM (développement sans `RUNNER_URL`) garde `PUBLIC_URL`.
- Le port `127.0.0.1:5202` est réservé dans la VM.
