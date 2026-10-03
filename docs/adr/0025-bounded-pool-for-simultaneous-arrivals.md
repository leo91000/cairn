# Pool borné pour les arrivées simultanées

Date : 2026-10-03. Statut : pool qualifié sur deux rafales en production ;
l'objectif de trois secondes reste à atteindre.

## Problème et décision

Les snapshots de v0.52.6 partagent la RAM et évitent le boot ordinaire. Une seule
VM anonyme est pourtant prête : trois conversations d'une rafale de quatre
copient encore leur journal et renouvellent Codex dans leur chemin critique.
Les restaurations se chevauchent déjà ; les sérialiser davantage ne résout rien.

Le pool accepte une cible explicite de une à quatre VM, par défaut une.
`LEO_READY_VM_POOL_SIZE` est conservé par le déploiement Compose ; le superviseur
utilise `readyVmPoolSize`. La préparation reste séquentielle en arrière-plan.
Chaque VM est anonyme, sans accès, attribuée une seule fois et réutilise les
garanties de renouvellement d'identité du snapshot.

Les VM préparées occupent les vrais slots et leur RAM compte dans le cgroup.
Pour ajouter un spare, leur coût résident mesuré plus 2 Gio de marge doit tenir
dans un quart du budget mémoire du nœud, borné entre 2 et 8 Gio. Le pool réévalue
aussi son coût après préparation et retire les plus anciennes VM excédentaires.
Le premier spare garde la politique existante sur les petits nœuds. Les seuils
de pression et le plafond mémoire partagé restent applicables à tout le pool.

Le travail actif évince d'abord les conversations retenues les plus anciennes
dont la publication est acquittée, puis les spares les plus anciens. Il n'évince
jamais une conversation non publiée pour agrandir le pool. Une préparation
annulée libère son slot uniquement après arrêt de son VMM et de son backend.
Le pool ne promet pas quatre hits quand les ressources ou les slots manquent.

## Diagnostic de production avant changement

Quatre nouvelles conversations, même agent/compte/modèle, après déploiement
v0.52.6. Le délai se termine au premier appel effectif du modèle, après le
préchauffage `generate=false`, pas à la première activité affichée.

| Phase | Hors pool, trois conversations | Hit du pool |
| --- | ---: | ---: |
| Admission manager | 1,32–4,67 s | 0,17 s |
| Préparation disque et VM sur le nœud | 3,30–3,56 s | 0,025 s |
| Copie du journal, incluse dans la ligne précédente | 1,22–1,42 s | aucune |
| Identité/Codex et montage workspace, inclus dans la préparation | 1,22–1,41 s | déjà fait |
| Requête guest vers premier appel modèle | 3,90–4,53 s | 2,98 s |
| Préchauffage WebSocket, inclus dans la ligne précédente | 1,69–1,83 s | 1,55 s |
| Délai total avant modèle | 10,54–13,29 s | 3,65 s |

Ces phases incluent des sous-phases : ne pas additionner toutes les lignes.
La copie est corrélée au début de boot par leurs timestamps adjacents ; les
identifiants physiques des environnements relient les trois traces de boot aux
conversations. Ce relevé n'avait pas de capture CPU : il ne prouve ni une
saturation CPU ni une cause réseau pour le temps restant dans Codex.

Une seconde rafale, avec échantillonnage commencé avant les envois, confirme
4,26 s pour le hit et 11,27–12,78 s pour les trois autres. Sur la fenêtre de
13,03 s qui couvre les quatre premiers appels, le cgroup utilise en moyenne
2,28 cœurs sur sept ; aucun des 131 intervalles CPU n'est bridé. Le compteur
PSI `io.some` augmente de 9,34 s, celui de l'hôte de 8,78 s : au moins une tâche
attend une I/O pendant une grande partie de la fenêtre. Ce n'est pas un délai
attribuable à chaque conversation ni une preuve causale isolée. L'admission
des trois misses reste espacée à 1,35 ; 2,86 et 4,36 s après leurs envois.

## Premier comparatif natif

Même image prototype, Firecracker 1.17, ublk Async, vrai Codex, enveloppe de
7 CPU/36 Gio et huit slots. Compte et modèle synthétiques ; stockage HTTP local.
Quatre requêtes simultanées, pool rempli avant chaque essai.

| Cible du pool | Délais avant modèle des quatre arrivées |
| --- | --- |
| 1 | 3,404 ; 3,691 ; 3,585 ; 3,409 s |
| 4 | 1,891 ; 1,895 ; 2,009 ; 1,893 s |

La médiane baisse de 45,8 %. Les huit conversations exécutent leur outil,
publient leurs blocs vérifiés et reprennent le même thread après SIGKILL du
contrôleur. Ce banc ne mesure ni l'admission du manager ni S3/WAN. Il ne valide
donc pas encore l'objectif de moins de trois secondes en production.

Un deuxième essai avec les timings supplémentaires donne 2,173–2,175 s pour
les quatre arrivées. Il vérifie que leurs identifiants sont ceux des quatre VM
déjà préparées. Le pool plein, template et caches inclus, occupe 2,93 Gio dans
le cgroup avant les envois. Les quatre publications et reprises après crash
passent également, avec 194 blocs vérifiés sur les quatre disques.

Les mesures distinguent les quatre VMM des conversations des spares reconstitués
pendant leur exécution. Le cgroup inclut tous les VMM, les backends et le cache.
Les traces supplémentaires séparent l'attente du verrou de préparation manager,
la préemption du remplissage du pool, le renouvellement réseau/entropie et le
redémarrage natif. La trace du clone porte désormais l'identifiant de son
environnement, pour éviter une attribution ambiguë au template commun.

## Qualification de l'image et comparaison en production

L'image de la révision `4c336c740451ea677c617ad648dd193c8ba6da70`, publiée
après tous ses contrôles CI, passe le test natif de quatre VM préparées :
2,332–2,334 s avant modèle, quatre publications avec vérification des blocs,
puis quatre reprises du même thread après SIGKILL et renouvellement des accès.
La CI de PR passe également après une relance du test de stockage : son premier
essai avait expiré en attendant les métriques du guest, alors que le même test
passait sur l'image publiée. Aucun timeout produit n'a été modifié.

Le pilote utilise cette image immuable sur les deux nœuds ; seul le VPS passe
à quatre spares. Les anciens conteneurs sont remplacés, le pool est rempli et
aucun tour n'est actif avant les envois. Le même agent dédié, compte et modèle
sont utilisés pour deux rafales de quatre messages simultanés. L'échantillonnage
CPU commence avant les messages. Les huit VM revendiquées sont exactement les
VM déjà présentes avant leurs rafales ; aucune restauration ni relance native
du snapshot n'a lieu dans leur chemin critique.

| Rafale | Pool à une VM, v0.52.6 | Pool à quatre VM, pilote |
| --- | --- | --- |
| Première | 10,537 ; 3,649 ; 13,285 ; 12,405 s | 10,590 ; 11,550 ; 8,793 ; 7,464 s |
| Seconde | 12,776 ; 4,257 ; 11,863 ; 11,267 s | 8,998 ; 7,918 ; 7,875 ; 4,754 s |

Sur ces huit arrivées par configuration, la médiane passe de 11,565 à
8,356 s (−27,8 %), la moyenne de 10,005 à 8,493 s (−15,1 %), et le maximum
de 13,285 à 11,550 s. Ce sont deux petites rafales par configuration, pas
une estimation du p99 ni un A/B simultané à charge extérieure contrôlée.
Le meilleur hit du pool à une VM reste plus rapide que le meilleur de ces
rafales à quatre spares : l'amélioration des misses ne garantit pas un gain
sur chaque arrivée. Une release ne doit pas annoncer l'objectif des trois
secondes comme atteint.

La seconde rafale du pilote se décompose ainsi, par message :

| Phase | A | B | C | D |
| --- | ---: | ---: | ---: | ---: |
| Envoi UI → début du run manager | 4,377 s | 3,011 s | 0,792 s | 0,172 s |
| Préparation manager → placement engagé | 0,247 s | 0,293 s | 1,048 s | 0,256 s |
| Placement → requête au nœud | 0,068 s | 0,060 s | 0,258 s | 0,043 s |
| Attribution de la VM/disque préparés | 0,039 s | 0,049 s | 0,071 s | 0,023 s |
| Horloge, accès et imports guest | 0,309 s | 0,413 s | 0,426 s | 0,330 s |
| Requête guest → premier appel modèle | 3,958 s | 4,092 s | 5,280 s | 3,930 s |
| Total avant modèle | 8,998 s | 7,918 s | 7,875 s | 4,754 s |

Dans la dernière phase, les traces du processus propre à chaque tentative
mesurent séparément la préparation du toolkit (0,632–1,078 s), l'obtention
des accès (19–28 ms), le login natif (46–73 ms), `thread/start` (446–703 ms)
et le préchauffage Codex (1,650–3,413 s). Ces sous-phases se chevauchent
partiellement ; ne pas les ajouter à la table précédente. Le toolkit refait
les liens de toolchains et appelle `mise reshim` puis `mise env` à chaque entrée.
Les seuls logs de console VM ne contiennent pas les RPC du processus de tour :
la décomposition les lit aussi dans le journal de sa tentative, avec une
liste stricte de noms d'opérations et de compteurs.

Les deux fenêtres utilisent en moyenne 1,79 puis 2,21 cœurs sur sept,
sans période bridée par le quota. Le PSI I/O `some` augmente respectivement
de 10,89 et 7,88 s : cela signale une attente I/O d'au moins une tâche,
pas un ralentissement de cette durée pour chaque conversation. La première
rafale suit de près le déploiement ; la seconde attend la fin des publications
et le remplissage du pool. La causalité du remplissage anonyme concurrent
n'est pas isolée par ces mesures.

Les huit tours réussissent, avec les huit révisions de sauvegarde acquittées.
Seules ces conversations synthétiques sont ensuite mises à la corbeille,
après vérification de leur propriétaire, du message, de l'absence de travail
en attente et de l'acquittement. La limite suivante est l'admission manager :
plusieurs essais reportent le lancement pendant le verrou de préparation,
mais les délais UI → manager incluent aussi la création durable du run et
les passes du scheduler. Les instrumenter et raccourcir cette fenêtre dans
une PR séparée est nécessaire avant d'attribuer tout ce temps au verrou.
Le toolkit et le préchauffage natif sont les autres postes restant à traiter.

Les résultats bruts et scripts de qualification restent hors de Git et seront
joints aux assets de release. Aucun dump de benchmark n'est ajouté ici.
