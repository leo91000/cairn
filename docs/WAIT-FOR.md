# Explorer `wait_for`

`wait_for` est l’outil unique d’attente durable : durée, date, fin d’une CI,
résultat d’une tâche ou réponse d’un service. Cairn peut effectuer les
vérifications pendant que la conversation libère son exécution, puis reprendre le
travail autorisé avec un résultat précis.

## Ce que cette PR permet déjà

Cairn fournit les conditions temporelles `delay` et `at`, ainsi que `run_finished`
et `task_finished` pour les conversations qui peuvent déléguer ;
les plugins accordés à l’agent ajoutent les leurs,
de type `<plugin>_<nom>` (voir les [conditions de plugins](PLUGINS.md#conditions)).
Le plugin GitHub livré en ajoute deux :

- `delay` : reprendre après 1 à 86 400 secondes.
- `at` : reprendre à une date RFC 3339 avec fuseau explicite (`Z` ou décalage), au
  plus 24 heures dans le futur. Une date passée renvoie un résultat immédiatement.
  Les fractions de seconde sont arrondies à la milliseconde supérieure pour ne
  jamais programmer une reprise avant la date demandée.
- `run_finished` : une exécution Cairn autorisée atteint un état terminal. Une
  conversation dont le tour a réussi mais qui possède encore un réveil ou des
  messages à traiter reste une dépendance en attente. Cette source exige l’accès
  à l’installation et peut viser les exécutions d’autres agents de cette
  installation, selon la délégation du propriétaire décrite dans
  [l’accès agent](AGENT-ACCESS.md).
- `task_finished` : une tâche que cette conversation a déléguée avec
  `delegate_task` ne travaille plus : son tour est fini sans réveil ni message à
  traiter, ou elle est en pause, arrêtée ou supprimée. Cette source exige la
  délégation de tâches et ne vise que les tâches de la conversation qui attend.
  Le rapport se lit ensuite avec `task_status` (voir les
  [tâches déléguées](adr/0059-delegated-tasks.md)).
- `github_workflow` : un workflow GitHub Actions identifié par un projet autorisé,
  un identifiant d’exécution et le SHA complet du commit termine. Le résultat
  inclut sa conclusion et son numéro de tentative. Le compte GitHub accordé à
  l’agent est utilisé ; son jeton reste dans l’installation.
- `github_pr_checks` : les checks requis d’une PR sont observés sur son head
  exact. Les protections classiques et les règles actives de branche fournissent
  les exigences, y compris les checks qui n’ont pas encore publié de résultat.
  Un échec ou une annulation réveille immédiatement ; la réussite exige que tous
  les checks requis soient satisfaits. Les checks facultatifs restent hors de
  cette condition.

## Attendre une durée ou une date

```json
{
  "condition": { "type": "delay", "seconds": 300 },
  "reason": "Revérifier le déploiement après cinq minutes"
}
```

```json
{
  "condition": { "type": "at", "date": "2026-10-23T09:00:00+02:00" },
  "reason": "Reprendre à neuf heures"
}
```

Les conditions temporelles ne prennent pas de `timeoutSeconds`. Atteindre l’heure
produit `{ "state": "completed", "type": "time", "wakeAt": … }`, tandis qu’un
état externe non résolu dans son délai produit `state: timed_out`. Les conditions
externes gardent un `timeoutSeconds` obligatoire de 1 à 86 400 secondes.

`sleep` disparaît de la liste des outils et de leur dispatcher. Les attentes
qu’il a déjà enregistrées continuent à fonctionner après redémarrage. Les nouvelles
attentes temporelles gardent les champs historiques du minuteur, sans `condition` ;
un champ optionnel `timeCondition` conserve la demande canonique pour distinguer
les répétitions des remplacements entre durée et date. Les anciens binaires
ignorent ce champ et reprennent au même `wakeAt` ; les nouveaux lisent aussi les
anciens minuteurs. L’epoch de compatibilité des données reste donc inchangée.
Les événements historiques `chat.sleep` restent lisibles sur le web et Android.

## Attendre un résultat externe

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

Exemple pour les checks requis d’une PR :

```json
{
  "condition": {
    "type": "github_pr_checks",
    "projectId": "22222222-2222-4222-8222-222222222222",
    "prNumber": 193,
    "headSha": "1111111111111111111111111111111111111111"
  },
  "timeoutSeconds": 1800,
  "reason": "Reprendre après les checks requis du commit à relire"
}
```

Un nouveau head produit `state: superseded`, avec le SHA actuel. Une PR fermée
ou mergée produit `state: closed`, avec son état GitHub. Ces observations
interrompent l’attente de l’ancien commit. L’absence de checks requis produit
`conclusion: no_required_checks` ; elle ne prouve pas que la CI a réussi. Une PR
peut avoir de nombreux jobs facultatifs, comme Cairn sans checks imposés par des
protections de branche. Leur vérification reste nécessaire avant un merge demandé.

Les résultats listent au plus 100 checks requis, présents et manquants confondus,
et indiquent une troncature ; les comptes et la décision utilisent l’ensemble
observé. Les échecs sont prioritaires dans les détails. Les conclusions GitHub
`SUCCESS`, `NEUTRAL` et `SKIPPED` satisfont un check requis. Les checks manquants,
`EXPECTED` et en cours gardent l’attente. Les règles de workflows obligatoires
qui ne sont pas des status checks ne sont pas prises en charge : elles donnent
une observation d’indisponibilité plutôt qu’une réussite supposée.
Un legacy status ne fournit pas son identité d’application. Si son nom correspond
à plusieurs exigences d’applications, son attribution est ambiguë : la condition
signale aussi une indisponibilité et demande une vérification directe de la PR.

Une conclusion `failure`, `cancelled` ou `timed_out` est aussi un résultat utile :
elle réveille l’agent pour qu’il puisse diagnostiquer le problème. Attendre
uniquement un succès masquerait les échecs jusqu’à l’expiration du délai.

## Gain par rapport à une boucle de vérification

Pour une attente illustrative de 20 minutes :

| Approche | Travail du LLM pendant l’attente | Vérifications de la source | Ressources de la conversation |
| --- | --- | --- | --- |
| Commande de statut toutes les 60 secondes | Environ 20 reprises d’inférence pour décider d’attendre encore | Environ 20 appels | L’exécution reste active si le processus attend dans la VM |
| `wait_for` avec `delay: 60`, puis vérification | Environ 20 continuations et vérifications | Environ 20 appels | Libérées entre les continuations |
| `wait_for` avec condition externe | Une inscription, puis une continuation avec le résultat | Effectuées par Cairn ; environ 40 vérifications avec une cadence de 30 secondes | Libérées pendant l’attente |

Ces comptes illustrent le mécanisme ; ils ne mesurent ni des tokens ni un prix.
Le gain principal est de retirer l’inférence et la VM de la boucle d’attente.
Les requêtes nécessaires pour observer le service existent toujours.

Une dépendance Cairn est vérifiée par le scheduler existant, normalement chaque
seconde. Les conditions de plugins sont vérifiées par un observateur séparé,
normalement toutes les 30 secondes. Le temps réel de reprise dépend aussi de la libération de
l’exécution précédente, des comptes et de la capacité de la node.

Le test du worker avec un seul créneau vérifie que la conversation se termine,
laisse sa dépendance s’exécuter, puis reprend la même session avant son minuteur
de secours. Il utilise le transport natif de fixture ; la rétention et l’éviction
des VM suivent la politique existante des réveils durables.

## Possibilités et préconditions

| Cas | Ce que cela permettrait | État et travail nécessaire |
| --- | --- | --- |
| Exécution Cairn terminée | Enchaîner une tâche après une dépendance et libérer un créneau pour elle | Disponible, avec accès à l’installation, y compris pour les exécutions d’autres agents |
| Workflow GitHub terminé | Reprendre pour analyser la réussite, les logs d’un échec ou une annulation | Disponible, avec projet, run ID et commit précis |
| Checks requis d’une PR | Reprendre quand les contrôles requis du commit sont satisfaits ou sur un premier échec | Disponible ; exigences des protections classiques et règles actives, checks manquants inclus |
| Nouveau commit sur une PR | Abandonner une attente devenue obsolète et vérifier le nouveau commit | Disponible pour `github_pr_checks`, avec résultat `superseded` explicite |
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
| Date ou retry après délai | Continuer après une heure donnée ou un délai de service | Disponible avec `wait_for` et ses conditions `delay` / `at` ; les tâches planifiées servent aux travaux récurrents |
| Processus local ou fichier produit dans la VM courante | Attendre une commande locale sans interrompre son travail | Utiliser l’attente native ; une attente qui libère la VM exige un job ayant sa propre exécution |

Cette matrice sépare les capacités livrées des extensions à concevoir. Une
condition sert à observer un état. Les actions qui suivent restent dans le
périmètre de la demande initiale et des permissions courantes.

## Contrat de reprise

- Un seul réveil par conversation ou exécution de mission. Une répétition identique conserve l’échéance ;
  une autre demande remplace la précédente, y compris entre durée, date et résultat.
- Le timeout est obligatoire pour une condition externe, entre 1 seconde et
  24 heures, et interdit pour les conditions temporelles. Dans une attente
  conditionnelle, `wakeAt` représente l’échéance de secours, pas l’heure minimale
  de reprise. Un résultat disponible peut provoquer une reprise plus tôt.
- Les résultats indiquent `completed`, `timed_out`, `unavailable`, `superseded`
  ou `closed`. Une reprise
  apporte une observation et demande de vérifier l’état actuel avant d’agir.
- Un nouveau message, un arrêt, une annulation, une suppression, un échec terminal
  ou une reprise manuelle d’un run invalide son ancien réveil. La pause de file
  conserve la demande et son échéance.
- L’inscription capture ses contrôles initiaux et la révision des messages dans
  une transaction courte, sans requête réseau. Elle revalide le grant après une
  lecture GitHub et vérifie aussi les messages déjà délivrés pendant cette
  lecture. Le callback ne peut pas restaurer
  une attente annulée ou remplacée.
- Le lancement revalide le réveil, les permissions et l’observation dans sa
  transaction. Le résultat ne devient jamais un faux message utilisateur.
- Les attentes redémarrent depuis les données persistées. L’expiration reste
  utilisable si la source externe est momentanément inaccessible.

Les erreurs temporaires d’un plugin maintiennent l’attente jusqu’à la prochaine
vérification ou au délai de secours. Un changement d’origine du projet, une
identité de workflow incompatible ou le retrait de l’instance accordée à l’agent
produit une observation d’indisponibilité. Une réduction des permissions de
l’agent bloque la préparation de sa reprise.

## Observer GitHub et préparer les prochaines sources

Le plugin GitHub utilise le compte accordé à l’agent, sans configuration de webhook.
Les observateurs identiques d’un même agent et des mêmes permissions et de la même
origine de projet partagent une vérification. Chaque lot contient au plus
16 cibles et quatre observations simultanées, chacune dans un worker Luau ; un lot
est lancé toutes les 30 secondes, sauf délai plus long demandé par le plugin. Cela borne la surveillance périodique à environ 1 920 observations
par heure. Un workflow requiert un appel ; une PR en requiert normalement quatre,
au plus dix avec pagination : 500 checks, 500 exigences distinctes et moins de
200 règles de branche. Une observation est bornée à 40 secondes au total.
Les appels revalident les règles, le head et l’état de la PR, sans bloquer
l’admission. Les inscriptions et les autres usages de GitHub s’ajoutent à ce budget.
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
responsabilité du scheduler Cairn. Les conditions livrées utilisent le
protocole ordinaire des outils MCP.

## Mesures et suite proposée

Les [mesures reproductibles](WAIT-FOR-MEASUREMENTS.md) comparent les métadonnées
et les lectures/écritures à la référence `sleep`. Les latences réseau GitHub et
la capacité de production sont distinctes de ces mesures locales.

La suite proposée est maintenant les groupes de dépendances. Les autres sources peuvent suivre
les usages observés, avec un identifiant stable, une lecture autorisée, un
résultat explicite et un timeout. Le Fil distingue désormais une conversation
différée dans sa section « Waiting » (« Reprise prévue » sur Android), avec une
carte unique par attente dans la conversation. L’attente ne commence qu’à la fin
du tour : sa carte suit donc ce que l’agent a fait après l’appel, et sa raison est
limitée à trois lignes, dépliables. Les notifications doivent encore la distinguer
d’un travail achevé.


## Missions

L’outil est également disponible pendant les missions. Une attente différée
reste attachée à l’exécution de la mission, sans créer de conversation cachée.
Après la fin réussie du tour, le statut devient `waiting`, sans `finishedAt` ni
résultat final. La mission conserve son emplacement actif : aucun second lancement
de cette même mission n’est admis pendant l’attente.

Cairn arrête et clôture l’exécution précédente, libère son compte et ses jetons,
puis observe la condition sans LLM. À l’échéance ou au résultat, la même exécution
repart avec sa session et son espace de travail, une nouvelle identité de tour
et les données observées. Le temps passé en attente ne consomme pas le budget
d’exécution restant. Une fin de tour en erreur ou une annulation supprime l’attente.

Arrêter la mission annule l’attente. Mettre sa **planification** en pause ne touche
que ses futurs lancements. Les outils de questions restent réservés aux conversations
interactives ; une mission qui nécessite une décision utilise `report_outcome`.
Le web, Android, le MCP public et `cairn_installation` exposent `waiting`, son
`wakeup` et l’annulation via les interfaces d’exécution existantes.
