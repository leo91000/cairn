# Un seul pool de comptes pour tous les agents de code

Codex et Claude Code partagent désormais le même modèle de compte d’agent de code. Un seul pool se charge de la création, de la pause, de la limite d’exécutions parallèles, de la sélection du compte ayant le plus de capacité restante, des baux d’exécution, de la bascule en cas d’usage épuisé et de l’orchestration de la connexion. Ce qui diffère réellement se trouve derrière un pilote par agent de code : le protocole de connexion de la CLI officielle, le stockage des identifiants, la lecture de l’usage et la remise des jetons à une exécution. Codex peut ainsi avoir plusieurs comptes, et Claude Code aussi, avec les mêmes règles et la même interface.

Les autres options étaient de garder deux implémentations parallèles, ce qui dupliquait la sélection, les limites et l’interface tout en figeant Claude Code à un seul compte, ou de tout aplatir dans une abstraction uniforme. Cette dernière aurait masqué des différences réelles : les réinitialisations en réserve n’existent que chez Codex, et l’usage de Claude Code peut être indisponible sans bloquer les exécutions. Ces différences restent des capacités du pilote.

Conséquences difficiles à inverser :

- Les comptes des deux agents de code sont stockés dans une seule collection. Les anciens comptes Codex et l’ancienne connexion Claude Code y sont migrés au démarrage, une seule fois, et les exécutions référencent leur compte par un identifiant neutre (`accountId`). Revenir à une version antérieure ferait disparaître ces comptes : il faudrait restaurer la sauvegarde prise avant la mise à jour.
- La session native de Claude Code appartient à l’exécution et non plus au compte : une conversation peut donc reprendre sur un autre compte Claude. Les sessions des anciennes exécutions locales sont copiées dans leur exécution lors de la migration.
- La remise des identifiants Claude par transfert sérialisé (`sync-required`), antérieure au broker d’accès, est supprimée. Une connexion encore marquée par ce transfert est migrée à l’état « À reconnecter ».
- Les anciennes routes `/api/codex/accounts` et `/api/claude/connection` sont remplacées par `/api/accounts`. Le serveur et l’application Android sont publiés avec la même étiquette.

Voir [les comptes d’agents de code](../AGENT-ACCOUNTS.md).
