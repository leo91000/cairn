# Démarrage authentifié : mesure du chemin réel

Mesures du 12 septembre 2026. Elles complètent et précisent les
[benchmarks d'infrastructure](STARTUP-STRATEGIES-BENCHMARK.md).

## Résultat principal

**Préparer les VM ne suffit pas encore pour atteindre 500 ms au premier appel
authentifié avec toutes les connexions actuelles.**

Sur trois VM neuves, dont les outils et Codex avaient été chauffés sans compte,
le premier prompt réel est accepté par Codex en **736 ms de médiane**. Tous les
MCP sont prêts en **1,48 s**. Sur une VM ayant déjà servi des appels authentifiés,
avec un nouveau processus Codex à chaque fois, ces temps tombent à **439 ms** et
**964 ms** respectivement.

Le premier texte est beaucoup plus variable : **6,65 s** de médiane pour ces
premiers appels, entre **4,37 et 14,38 s**. Le modèle utilisé est celui sélectionné
par défaut dans les essais : **gpt-6-astra**, raisonnement **high**.

## Comparaison des chemins mesurés

| Chemin | Prompt/tour accepté | Tous les MCP prêts | Premier texte |
| --- | ---: | ---: | ---: |
| Manager de production, trois petites tâches sans projet | 4 586 ms | Non instrumenté | 9 360 ms |
| VM neuve préparée anonymement, premier appel authentifié, n=3 | 736 ms | 1 481 ms | 6 652 ms |
| VM déjà utilisée, nouveau Codex, connexions habituelles, n=5 | 439 ms | 964 ms | 5 017 ms |

Ce sont des médianes, pas une garantie de délai maximal. Le premier chemin part
de la création de l'exécution dans le manager et utilise ses événements stockés.
Les deux autres partent de la requête du contrôleur vers la VM disponible. Ils
incluent la configuration transmise au guest, le nouveau Codex, le jeton réel,
l'authentification, les modèles, le thread et le tour, mais excluent la file du
manager, les projets et le trajet navigateur/manager.

`turn/start` est un accusé de réception local de Codex. Les notifications montrent
que des MCP démarrent encore après cet accusé de réception. Nous n'avons pas
horodaté le départ de la requête HTTPS vers le fournisseur : **« accepté par
Codex » ne signifie pas « le fournisseur génère déjà »**.
Source : [protocole Codex app-server](https://learn.chatgpt.com/docs/app-server#api-overview).

Les trois exécutions de production sont des tâches no-op utilisant le même
`chat_process` et le même app-server que les chats. Elles ne constituent pas un
test d'envoi depuis l'interface de chat ni une mesure du rendu dans le navigateur.

## Les coûts identifiés

### 1. Le worker attend son prochain passage

L'attente entre création de l'exécution et attribution du compte a été de
**261, 914 et 949 ms** dans les trois tâches courtes. Elle combine file et
sélection de compte : aucune instrumentation du manager n'a été installée pour
séparer ces deux opérations.

Le code vérifie les nouvelles demandes sur un intervalle d'**une seconde**,
sans réveil immédiat depuis les routes de création examinées. Il faudra notifier
le worker à l'envoi d'un message, tout en gardant le timer pour les tâches
planifiées et la récupération. Diminuer le coût de boot ne retire pas cette attente.
Source : [worker](../crates/installation/src/worker.rs).

### 2. La découverte des modèles se répète au premier compte

Après préchauffage anonyme, `initialize` prend environ 167–185 ms et le login
externe environ 55–61 ms. Mais le premier `model/list` authentifié prend encore
**337–418 ms**, médiane **378 ms**. Les appels suivants sur la même VM sont
généralement de quelques millisecondes.

Cela explique pourquoi les 216 ms du benchmark sans compte ne deviennent pas
automatiquement un premier chat réel sous 500 ms. Une prochaine implémentation
devrait exploiter les métadonnées déjà découvertes par le manager, liées au compte
et au modèle, avec invalidation appropriée. Il faut ensuite mesurer si l'on retire
réellement cette requête du chemin critique, sans perdre les contrôles de capacités.
Sources : [démarrage du chat](../crates/installation/src/chat_process.rs),
[gestion des comptes](../crates/installation/src/accounts.rs).

### 3. Les applications connectées ajoutent un second MCP

Le compte initialise automatiquement **`codex_apps`, avec 257 outils**, en plus
du MCP Agent Manager configuré dans l'application, qui expose **20 outils** sur
la version de production testée. Désactiver uniquement le MCP déclaré par le
manager dans le contrôle expérimental laisse donc les applications connectées
actives.

Sur les mesures détaillées, le MCP Agent Manager passe de `starting` à `ready`
en quelques dizaines de millisecondes, tandis que `codex_apps` ajoute plusieurs
centaines de millisecondes, jusqu'à environ 1,3 seconde dans un premier appel.

Un contrôle avec **`features.apps=false` uniquement dans les processus de test**,
en conservant Agent Manager, atteint tous les MCP prêts en **405–441 ms** sur les
deux appels après le premier. Ce n'est ni une modification de production, ni une
recommandation de supprimer globalement des outils utilisés. Une option par
agent, ou un chargement différé si le protocole le permet, mérite d'être étudié.
Source : [configuration officielle des applications](https://learn.chatgpt.com/docs/config-file/config-reference).

### 4. Le monitoring du compte peut bloquer son jeton

Sur les 15 appels authentifiés du guest, le relais du jeton prend **4,5 ms de
médiane**, mais un appel a pris **4 012 ms**. Le RPC de login lui-même prend
**57 ms de médiane**, entre **26 et 86 ms**.

Une série séparée de **275 lectures `refresh:false` pendant 70 secondes**,
effectuée directement dans le conteneur manager, donne **1,38 ms de médiane** et
un pic à **821 ms**. Pendant ce pic, la date `checkedAt` des quotas a avancé ; elle
est restée inchangée sur les lectures rapides voisines.

Le code prend le même verrou de compte pour lire les jetons et pour actualiser
les quotas, cette dernière opération englobant des échanges réseau et la
découverte des modèles. Cela étaye l'explication par contention. Le détail de
chacune des quatre secondes du premier pic n'a pas été instrumenté.

La correction à étudier est une lecture rapide des jetons encore valides qui
n'attende pas tout le monitoring, **en conservant une rotation sérialisée et la
validation de la réservation du compte**. Supprimer simplement le verrou serait
une régression du contrat d'authentification.
Sources : `access_tokens` et `refresh` dans
[accounts.rs](../crates/installation/src/accounts.rs).

## Ce qui reste hors de ces conclusions

- Les réponses réellement reçues étaient toutes `OK`. Le délai du modèle varie
  fortement même avec ce prompt minuscule ; les gains d'infrastructure ne
  garantissent pas une première réponse en 500 ms.
- Cinq requêtes `/health` depuis la machine de développement, chacune avec une
  connexion TLS neuve, prennent **95–140 ms**. Ce n'est pas la latence du téléphone
  ni celle d'une connexion navigateur réutilisée ; on ne l'ajoute pas mécaniquement
  aux autres chiffres.
- Aucun renouvellement de jeton n'a été forcé, aucun compte épuisé et aucun
  changement de compte en cours de génération n'a été induit. La méthode laisse le
  manager propriétaire de la rotation. Le pic de monitoring observé ne couvre
  donc pas tous les cas de renouvellement ou de reprise.
- Les tests ne démontrent pas une nouvelle implémentation complète du stock de VM,
  un p95/p99, ni le comportement après un pic de charge durable.

## Suite recommandée

Préparer et mettre en pause les VM reste utile. Pour viser 500 ms de préparation
réelle, il faut compléter ce changement par le réveil immédiat du worker, retirer
la découverte répétée des modèles du chemin critique lorsque le cache du compte
est valide, et empêcher le monitoring de bloquer inutilement la lecture du jeton.
Le chargement des applications connectées doit être une décision explicite qui
préserve les outils nécessaires à l'agent.

Ces points nécessitent une implémentation puis une nouvelle mesure intégrée. Les
chiffres actuels ne permettent pas de promettre 500 ms pour un premier chat avec
toutes les connexions activées.

## Preuves et nettoyage

[Données agrégées et échantillons](https://github.com/leo91000/cairn/releases/download/v0.50.6/authenticated-startup-2026-09-12.json).
Les scripts exploratoires sont conservés dans `/var/tmp/cairn-auth-bench/` sur la
machine de développement. La production reste au commit `8643deb`, sans changement
de paramètres ni redéploiement.

Un agent de benchmark sans projet, sans GitHub et sans skill a servi aux tâches
de contrôle. Le manager lui a attribué un vrai compte ; son broker authentifié
était utilisé à travers un relais Unix/vsock, sans copier de jeton dans les
scripts ou les résultats et sans créer un second propriétaire du refresh token.
Les VM utilisaient uniquement cette réservation et la configuration MCP de cette
exécution. Aucun outil MCP métier n'a été appelé pour modifier des données.

Les 15 tours du banc authentifié, les trois tâches courtes et la tâche de support
sont terminés. Les deux tâches de benchmark ont été archivées. Le conteneur
d'essai et ses disques privés ont été supprimés ; les historiques des exécutions
réelles restent dans le manager. La vérification finale donne `activeRuns: 0` et
Firecracker prêt.
