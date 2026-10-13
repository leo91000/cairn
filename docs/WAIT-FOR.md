# Explorer `wait_for`

`wait_for` vaut le coup quand une conversation attend un état vérifiable ailleurs :
fin d’une CI, résultat d’une tâche ou réponse d’un service. Cairn peut effectuer les
vérifications pendant que la conversation libère son exécution, puis reprendre le
travail autorisé avec un résultat précis.

## Ce que cette PR permet déjà

Deux conditions sont disponibles dans `cairn_workspace.wait_for` :

- `run_finished` : une exécution Cairn autorisée atteint un état terminal. Une
  conversation dont le tour a réussi mais qui possède encore un réveil ou des
  messages à traiter reste une dépendance en attente. Cette source exige l’accès
  à l’installation, le même agent et des permissions qui couvrent la cible.
- `github_workflow` : un workflow GitHub Actions identifié par un projet autorisé,
  un identifiant d’exécution et le SHA complet du commit termine. Le résultat
  inclut sa conclusion et son numéro de tentative. Les credentials existants de
  l’agent sont utilisés ; ils restent dans l’installation.

Exemple pour une dépendance Cairn :

```json
{
  "condition": {
    "type": "run_finished",
    "runId": "11111111-1111-4111-8111-111111111111"
  },
  "timeoutSeconds": 1800,
  "reason": "Reprendre après la vérification déléguée"
}
```

Exemple pour un workflow GitHub ; les identifiants sont illustratifs :

```json
{
  "condition": {
    "type": "github_workflow",
    "projectId": "22222222-2222-4222-8222-222222222222",
    "runId": 123456789,
    "headSha": "1111111111111111111111111111111111111111"
  },
  "timeoutSeconds": 1800,
  "reason": "Vérifier le résultat de la CI du commit poussé"
}
```

Si le résultat est déjà disponible, l’outil renvoie `deferred: false` et
`observation`. L’agent l’inspecte et continue son tour. Sinon, il reçoit
`deferred: true` et un réveil confirmé, puis termine son tour. Cairn reprend la
même conversation, session et environnement après observation du résultat ou
expiration du délai, une fois l’exécution précédente libérée et selon la capacité.

Une conclusion `failure`, `cancelled` ou `timed_out` est aussi un résultat utile :
elle réveille l’agent pour qu’il puisse diagnostiquer le problème. Attendre
uniquement un succès masquerait les échecs jusqu’à l’expiration du délai.

## Gain par rapport à une boucle de vérification

Pour une attente illustrative de 20 minutes :

| Approche | Travail du LLM pendant l’attente | Vérifications de la source | Ressources de la conversation |
| --- | --- | --- | --- |
| Commande de statut toutes les 60 secondes | Environ 20 reprises d’inférence pour décider d’attendre encore | Environ 20 appels | L’exécution reste active si le processus attend dans la VM |
| `sleep` de 60 secondes, puis vérification | Environ 20 continuations et vérifications | Environ 20 appels | Libérées entre les continuations |
| `wait_for` | Une inscription, puis une continuation avec le résultat | Effectuées par Cairn ; environ 40 vérifications avec une cadence de 30 secondes | Libérées pendant l’attente |

Ces comptes illustrent le mécanisme ; ils ne mesurent ni des tokens ni un prix.
Le gain principal est de retirer l’inférence et la VM de la boucle d’attente.
Les requêtes nécessaires pour observer le service existent toujours.

Une dépendance Cairn est vérifiée par le scheduler existant, normalement chaque
seconde. GitHub est vérifié par un observateur séparé, normalement toutes les
30 secondes. Le temps réel de reprise dépend aussi de la libération de
l’exécution précédente, des comptes et de la capacité de la node.

Le test du worker avec un seul créneau vérifie que la conversation se termine,
laisse sa dépendance s’exécuter, puis reprend la même session avant son minuteur
de secours. Il utilise le transport natif de fixture ; la rétention et l’éviction
des VM suivent la politique existante des réveils durables.

## Possibilités et préconditions

| Cas | Ce que cela permettrait | État et travail nécessaire |
| --- | --- | --- |
| Exécution Cairn terminée | Enchaîner une tâche après une dépendance et libérer un créneau pour elle | Disponible, pour le même agent et les ressources autorisées |
| Workflow GitHub terminé | Reprendre pour analyser la réussite, les logs d’un échec ou une annulation | Disponible, avec projet, run ID et commit précis |
| Tous les checks d’une PR | Reprendre quand tous les contrôles requis du commit sont terminés | Ajouter un adaptateur pour les checks requis ; un workflow seul ne représente pas toute la CI |
| Nouveau commit sur une PR | Abandonner une attente devenue obsolète et vérifier le nouveau commit | Ajouter une condition de changement de head, avec résultat `superseded` explicite |
| Review GitHub humaine | Reprendre après une décision de review sur le commit attendu | Définir les reviewers requis et traiter les approbations périmées ou révoquées |
| Review d’un agent indépendant | Reprendre après la fin de la review et lire les findings | Ajouter une source durable de fin de sous-agent et sa délégation d’accès ; les outils natifs de collaboration ne sont pas des runs Cairn |
| Plusieurs dépendances | Attendre tout un groupe ou la première réponse utile | Ajouter des conditions `all`/`any`, des limites de taille et une politique sur les échecs |
| Chaîne de tâches | Construire, vérifier puis publier dans des exécutions distinctes | Les dépendances simples sont disponibles ; les cycles sont rejetés et les chaînes sont bornées à 32 runs |
| Réponse utilisateur | Suspendre la conversation jusqu’à une réponse, sans garder sa VM active | Prolonger les questions durables existantes et conserver leur état à la fin du tour |
| Approbation explicite | Reprendre lorsqu’une action préparée est autorisée | Lier la réponse à l’action exacte et à sa version ; le réveil doit transporter la décision vérifiée |
| Build ou commande longue | Attendre un job qui continue indépendamment du tour | Créer d’abord des jobs gérés avec identifiants, résultat et annulation ; ils gardent leurs propres ressources |
| Déploiement terminé | Vérifier l’environnement et la version attendus puis continuer | Ajouter une source de statut de déploiement, liée au commit et à l’environnement |
| Service prêt | Reprendre après disponibilité d’un service ou d’une version | Ajouter un adaptateur à endpoint fixe, avec auth et critères de disponibilité définis |
| Import/export terminé | Enchaîner après un travail exécuté par un service externe | Ajouter un adaptateur de job avec identité et résultat durables |
| Artifact disponible | Reprendre après publication ou traitement d’un fichier | Observer un artifact géré ou le job qui le produit, avec ses permissions |
| Message d’un plugin ou connecteur | Inspecter un événement reçu, par exemple dans une intégration de discussion | Nécessite une source installée et autorisée, un événement identifié et une politique sur les données reçues |
| Credential reconnecté | Continuer après intervention du propriétaire | La file des comptes gère déjà une partie de ce cas ; une attente dédiée ne doit pas accorder de nouveaux droits |
| Capacité ou node disponible | Reprendre quand un travail peut être admis | L’admission et le placement le gèrent déjà ; une observation ne remplace pas une réservation |
| Date ou retry après délai | Continuer après une heure donnée ou un délai de service | `sleep` et les tâches planifiées restent adaptés aux conditions purement temporelles |
| Processus local ou fichier produit dans la VM courante | Attendre une commande locale sans interrompre son travail | Utiliser l’attente native ; une attente qui libère la VM exige un job ayant sa propre exécution |

Cette matrice sépare les capacités livrées des extensions à concevoir. Une
condition sert à observer un état. Les actions qui suivent restent dans le
périmètre de la demande initiale et des permissions courantes.

## Contrat de reprise

- Un seul réveil par conversation. Une répétition identique conserve l’échéance ;
  une autre demande remplace la précédente, y compris entre `sleep` et `wait_for`.
- Le timeout est obligatoire, entre 1 seconde et 24 heures. Dans une attente
  conditionnelle, `wakeAt` représente l’échéance de secours, pas l’heure minimale
  de reprise. Un résultat disponible peut provoquer une reprise plus tôt.
- Les résultats indiquent `completed`, `timed_out` ou `unavailable`. Une reprise
  apporte une observation et demande de vérifier l’état actuel avant d’agir.
- Un nouveau message, un arrêt, une annulation, une suppression, un échec terminal
  ou une reprise manuelle d’un run invalide son ancien réveil. La pause de file
  conserve la demande et son échéance.
- L’inscription revalide le grant après une lecture GitHub et vérifie aussi les
  messages déjà délivrés pendant cette lecture. Le callback ne peut pas restaurer
  une attente annulée ou remplacée.
- Le lancement revalide le réveil, les permissions et l’observation dans sa
  transaction. Le résultat ne devient jamais un faux message utilisateur.
- Les attentes redémarrent depuis les données persistées. L’expiration reste
  utilisable si la source externe est momentanément inaccessible.

Les erreurs temporaires de GitHub maintiennent l’attente jusqu’à la prochaine
vérification ou au délai de secours. Un changement d’origine du projet ou une
identité de workflow incompatible produit une observation d’indisponibilité.
Une réduction des permissions de l’agent bloque la préparation de sa reprise.

## Observer GitHub et préparer les prochaines sources

Le prototype réutilise la connexion GitHub existante, sans demander de nouvelle
configuration de webhook. Les observateurs identiques d’un même agent et des
mêmes permissions et de la même origine de projet partagent une vérification. Chaque lot contient au plus
16 cibles et quatre appels CLI simultanés ; un lot est lancé toutes les
30 secondes. Cela borne la surveillance périodique à environ 1 920 appels par
heure, auxquels s’ajoutent les inscriptions et les autres usages de GitHub.
Avec beaucoup de cibles, la reprise peut attendre plusieurs lots.

Les [webhooks GitHub](https://docs.github.com/en/webhooks/webhook-events-and-payloads)
offrent ensuite des événements de workflow, checks, reviews et déploiements.
Leur adoption demanderait une configuration autorisée, une réception adaptée aux
installations derrière NAT, la validation des signatures, la déduplication et une
vérification de rattrapage après interruption. Une source devrait toujours pouvoir
réévaluer son état actuel pour éviter de perdre un résultat entre inscription et
observation.

Les [Tasks MCP](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks)
de la spécification 2025-11-25, encore expérimentales, décrivent des identifiants
durables, des résultats différés et l’annulation. Elles pourraient servir aux
jobs gérés et à leur interopérabilité. Le réveil de la conversation reste une
responsabilité du scheduler Cairn. Les deux conditions livrées utilisent le
protocole ordinaire des outils MCP.

## Mesures et suite proposée

Les [mesures reproductibles](WAIT-FOR-MEASUREMENTS.md) comparent les métadonnées
et les lectures/écritures à la référence `sleep`. Les latences réseau GitHub et
la capacité de production sont distinctes de ces mesures locales.

La suite la plus utile serait une attente des checks requis d’une PR, liée à son
head exact, puis les groupes de dépendances. Les autres sources peuvent suivre
les usages observés, avec un identifiant stable, une lecture autorisée, un
résultat explicite et un timeout. L’état d’attente dans le Fil et les notifications
doit aussi distinguer une conversation différée d’un travail achevé.
