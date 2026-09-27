# Stockage des environnements et reprise à la demande

Statut : conception validée le 27 septembre 2026 à la question Q9 de l'entretien
`grill-with-docs`. L'utilisateur a confirmé les décisions Q1–Q8, la synthèse
technique, les interfaces de test et le découpage en quatre PR sur la PR des nodes.
Implémentation en cours ; la validation réelle FUSE/Firecracker reste requise.

## Base de travail

- PR des nodes : <https://github.com/leo91000/leo-agent-manager/pull/3>.
- Révision examinée : `360e714297e61242fb359284bedf22a648fcb99e`.
- Branche de préparation : `feat/on-demand-storage-design`.
- Le travail porte sur le stockage de l’environnement complet de la conversation,
  y compris ses fichiers non publiés, sa session et ses outils.
- La reprise à la demande évite le téléchargement préalable de tout le disque.
  Les premières lectures et le démarrage de l’agent conservent une latence.
- Les écritures restent locales, avec protection distante asynchrone et retard
  visible, conformément à l’ADR-0004. La perte de la node peut perdre le travail
  non sauvegardé.
- La livraison demandée est une stack de PR distinctes de la PR des nodes.

## Connaissances réutilisables

La PR des nodes contient déjà les droits d’exécution, le propriétaire unique de
la conversation, la capture coordonnée, des manifestes de blocs de 4 Mio, la
vérification d’intégrité, la publication et la rétention des sauvegardes de reprise.

L’ADR-0005 choisit `dm-era` dans l’invité pour accélérer la capture d’un disque
entièrement local. Ce suivi est consultatif et le disque brut reste lisible sans
lui. La reprise à la demande change cette hypothèse : les informations indiquant
quelles données sont disponibles localement deviennent nécessaires pour lire le
disque. Ce compromis devra être décidé et documenté explicitement.

L’autorisation de lecture actuelle expire après la restauration. Les lectures à
la demande exigent de gérer son renouvellement, sa révocation et la conservation
des objets encore utilisés par une exécution.

## Arbre des décisions

### Contraintes techniques vérifiées

- Firecracker et le jailer sont épinglés en version 1.17.0. Le montage actuel
  utilise un lien physique vers une image brute dans le jail. Un disque FUSE
  demanderait un montage accessible dans ce jail et une gestion de son cycle de vie.
- L'adapter recommandé pour un premier essai est un fichier brut virtuel FUSE,
  avec E/S directes. Ce choix reste soumis à un essai réel avec le jailer ; il
  ne constitue pas encore une décision validée pour la production.
- Les conteneurs disposent déjà de `SYS_ADMIN` et `MKNOD`, mais `/dev/fuse`
  n'est pas exposé. Le déploiement devra vérifier cette capacité, les permissions
  du montage et son nettoyage après un arrêt brutal.
- Les disques actuels omettent `cache_type`. Firecracker utilise alors `Unsafe`
  et ignore les demandes de flush invité. Le nouveau chemin devra utiliser
  `Writeback` et vérifier que `fsync` atteint le journal local durable. Le cache
  d'écriture FUSE ne devra pas acquitter les écritures avant leur réception par
  le module de stockage.
- Les manifestes version 1 avec blocs de 4 Mio et blocs nuls explicites peuvent
  être réutilisés. Les opérations qui manipulent directement `data.ext4`
  (export, archive, redimensionnement, inspection) devront passer par le module
  ou matérialiser explicitement une image complète.
- Les autorisations de lecture devront rester valides pendant l'exécution et
  les objets distants utilisés devront être protégés de la collecte.

L'essai devra mesurer la disponibilité de l'invité avec un cache vide, les octets
téléchargés et les lectures lentes. Il devra aussi vérifier `fsync` suivi d'un
arrêt brutal, la relecture d'un journal tronqué, les écritures concurrentes avec
un téléchargement, les générations de sauvegarde, la corruption et la panne S3.
Les E/S synchrones de Firecracker peuvent bloquer son traitement des périphériques
pendant une lecture distante : le comportement doit être mesuré.

Sources primaires : [disques Firecracker 1.17.0](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/block.md),
[E/S FUSE du noyau Linux 6.12](https://github.com/torvalds/linux/blob/v6.12/Documentation/filesystems/fuse-io.rst),
[configuration FUSE](https://github.com/torvalds/linux/blob/v6.12/Documentation/filesystems/fuse.rst).

### Tour 1 — décisions confirmées

1. **Cycle de vie** : le cache local est transparent. Sa libération laisse la
   conversation active, avec historique consultable et reprise automatique.
   L'archivage reste distinct et peut conduire au stockage froid.
2. **Activation et migration** : activation par node, puis migration automatique
   et progressive des environnements arrêtés. La copie distante doit être
   vérifiée avant toute libération de la copie locale.
3. **Indisponibilité du stockage** : une lecture manquante met l'exécution en
   attente, préserve son travail et reprend automatiquement au retour du stockage.
   L'attente reste annulable et conditionnée au droit d'exécution.

### Tour 2 — décisions confirmées

4. Désactiver par défaut l'archivage automatique sur les nodes activées ; conserver
   l'archivage manuel et la restauration des archives existantes.
5. Inclure la node locale du serveur principal et les nodes distantes dès la
   première version, avec activation explicite.
6. Configurer un budget de cache par node, évincer uniquement les données
   sauvegardées, puis mettre en pause avant saturation et reprendre automatiquement
   quand l'espace redevient disponible.
7. Viser une sauvegarde toutes les 60 secondes et mettre en pause au-delà de
   5 minutes depuis le dernier point distant ; seuils configurables et retard réel
   affiché.
8. Attendre le retour du stockage sans expiration arbitraire, avec annulation
   possible ; reprise automatique sous réserve du droit d'exécution et de
   l'intégrité des données.

La confirmation finale ci-dessous porte sur les détails proposés pour traduire
ces décisions en une implémentation vérifiable.

### Moyens de validation disponibles

L'environnement de développement expose `/dev/kvm` et `/dev/fuse` au 27 septembre
2026. Leur présence ne prouve pas encore que les permissions, les montages et le
jailer permettent l'essai complet ; cette validation reste à effectuer.

## Découpage proposé pour la stack

Chaque PR dépend de la précédente ; la première cible la branche de la PR #3.

1. **Interface de stockage et compatibilité** : regrouper les opérations sur les
   disques derrière le module de stockage, avec un adapter local conservant le
   fonctionnement existant. Vérifier création, lecture, écriture, export et
   redimensionnement. Conserver `dm-era` sur ce chemin.
2. **Reprise à la demande expérimentale** : adapter FUSE, journal local durable,
   reprise après arrêt brutal et démarrage réel avec cache vide. Fonctionnalité
   désactivée par défaut, réservée à l'essai jusqu'aux protections de la PR suivante.
   Le journal devient la source des changements pour ce nouvel adapter.
3. **Sauvegardes et exploitation bornée** : publication des générations,
   conservation des objets utilisés, autorisations renouvelables, budget disque,
   pauses/reprises et états visibles. Vérifier un déplacement entre deux nodes,
   y compris la node locale. Activation utilisable pour de nouveaux environnements.
4. **Migration et cycle de vie** : conversion progressive des environnements
   arrêtés, suppression de leur ancienne copie seulement après vérification,
   archivage automatique désactivé par défaut pour les nodes activées, archivage
   manuel et restauration des anciennes archives préservés.

L'ancien adapter reste disponible pour les nodes non activées et les disques non
migrés. Son retrait global n'est pas requis pour livrer cette stack.

Le découpage exact pourra évoluer pour que chaque PR ait un comportement vérifiable
et des garanties cohérentes. Aucune de ces PR n’est encore implémentée.

## Synthèse technique proposée pour confirmation finale

### Lecture et écriture

1. Un manifeste immuable décrit le disque distant par blocs de 4 Mio, avec une
   distinction explicite entre bloc nul et bloc distant absent du cache.
2. Une lecture superpose les écritures locales les plus récentes à la base.
   Seules les portions nécessaires non couvertes par le journal déclenchent la
   lecture du cache ou le téléchargement du bloc distant, avec contrôle d'intégrité.
   Une arrivée réseau tardive ne peut jamais écraser une écriture locale.
3. Une écriture partielle ajoute au journal sa séquence, son emplacement, ses
   données et une somme de contrôle, sans télécharger au préalable le bloc parent.
   L'acquittement suit la persistance locale ; les synchronisations demandées par
   l'invité sont propagées jusqu'au stockage persistant. Le regroupement de
   synchronisations est possible si cette garantie est conservée.
4. Après un arrêt brutal, la reprise valide le manifeste local et rejoue les
   enregistrements complets. Une fin de journal incomplète ne remet pas en cause
   les écritures déjà acquittées ; une corruption dans la partie durable bloque
   la reprise et produit une erreur explicite.

### Sauvegarde et libération de place

1. Coordonner le gel du système de fichiers invité alors que l'invité fonctionne,
   puis la pause de la VM et la fin des E/S engagées ; synchroniser et sceller la
   génération courante du journal. Installer la suivante avant reprise et dégel.
   Toute erreur doit défaire le gel et la pause lorsque c'est possible sans
   reprendre des écritures non protégées. Les étapes distantes restent hors de
   cette section critique.
2. Reconstituer les blocs modifiés à partir de la génération scellée et du parent ;
   réutiliser les blocs inchangés. Envoyer des objets immuables individuellement
   chiffrés et vérifiables, puis leur manifeste après confirmation de toutes les
   dépendances. Conserver la compatibilité de lecture des sauvegardes existantes.
3. Le master publie atomiquement le nouveau point sous contrôle du propriétaire
   et de sa génération. Un ancien propriétaire ne peut ni publier ni reprendre
   l'exécution après révocation de ses droits.
4. Compacter uniquement le journal couvert par une publication confirmée ;
   conserver toutes les écritures ultérieures. Protéger de la collecte les objets
   des points retenus, des disques montés et des publications en cours.
5. Le cache propre peut être évincé ; le journal non sauvegardé ne le peut pas.
   Les lectures, téléchargements, reconstructions et migrations doivent réserver
   leur espace temporaire dans le même comptage par node.

### Réglages initiaux proposés

- S3 accessible immédiatement est requis pour activer ce mode. Les objets
  référencés par un environnement actif doivent rester hors des classes froides.
  La configuration des cycles de vie du bucket fait partie de la vérification
  d'activation ; les sauvegardes locales historiques restent compatibles.
- Plafond du cache propre : **100 Gio par node**, configurable et automatiquement
  limité par l'espace réellement disponible. Ce plafond n'est pas une réservation
  et ne comprend pas le journal non sauvegardé, qui reste comptabilisé séparément.
- Réserve libre : **maximum de 10 Gio et 5 % du volume**, configurable. Le journal
  et les fichiers temporaires consomment la capacité au-dessus de cette réserve.
  L'admission d'une nouvelle écriture tient compte des écritures déjà engagées.
- Si la node est déjà sous la réserve, interdire les nouveaux travaux mais
  permettre la sauvegarde et la migration qui libèrent de l'espace, sous un budget
  temporaire strict basé sur l'espace réel. Ne pas créer une seconde image complète
  pour convertir un disque ; si même une conversion bornée manque de place,
  indiquer précisément la capacité supplémentaire nécessaire.
- Une migration à la fois par node, uniquement sur environnement arrêté et
  verrouillé contre une reprise concurrente. Une demande de reprise peut différer
  la migration et utiliser le disque initial jusqu'à la bascule atomique.
- Une sauvegarde est visée toutes les **60 s** quand des changements existent.
  Le seuil de **5 min** concerne le travail non sauvegardé : une conversation
  inactive et entièrement sauvegardée ne se met pas en erreur parce que son
  dernier point est ancien. Pour une reprise à partir d'un point ancien, le délai
  commence avec la première modification non sauvegardée.

### Attente, annulation et reprise

- Exposer la cause d'attente, le dernier point distant, le retard et l'occupation
  locale dans l'application. Une panne réseau transitoire conserve la lecture en
  attente avec nouvelles tentatives ; elle ne devient pas une lecture de zéros.
- Dépasser le seuil de retard ou manquer d'espace suspend les nouvelles écritures
  de l'invité. Le transfert des générations déjà scellées, l'éviction et le
  renouvellement des droits continuent pendant cette attente.
- Une demande de lecture bloquée ne doit pas empêcher l'annulation ni le contrôle
  des droits. La reprise automatique vérifie à nouveau toutes les causes de pause
  et utilise une marge de capacité pour éviter des pauses/reprises en boucle.
- L'annulation termine l'exécution et conserve le journal et la conversation.
  Elle ne promet pas de conserver les processus ou leur mémoire après arrêt de VM.
- Une corruption persistante ou une révocation des droits bloque la reprise et
  nécessite une résolution explicite ; elle ne déclenche pas une boucle infinie
  assimilée à une panne réseau temporaire.
- L'attente de stockage est visible dans l'application, sans notification externe
  ajoutée dans cette stack. Les délais existants d'inactivité et de déconnexion
  doivent distinguer cette attente d'une node réellement perdue.

### Interfaces de test et critères de livraison proposés

Deux niveaux de vérification : l'interface publique du module de stockage pour
les garanties sur les octets et la durabilité, puis le parcours conversation/VM
avec le vrai Firecracker et le jailer pour les garanties de reprise et de produit.

- Lire et écrire un disque partiellement distant donne les mêmes octets qu'un
  disque local de référence, y compris avec écritures qui chevauchent des blocs.
- Une petite écriture dans un bloc absent ne télécharge pas sa base avant acquittement.
- Une synchronisation acquittée résiste à un arrêt brutal du processus de stockage ;
  une publication interrompue ne rend jamais visible un point incomplet.
- Un téléchargement concurrent n'écrase aucune écriture ; sceller une génération
  et la publier ne supprime aucune écriture de la génération suivante.
- Une VM avec cache vide devient utilisable avant téléchargement de tout le disque.
  Le scénario contient des données volumineuses inutilisées, dont les objets ne
  doivent pas être lus pendant le démarrage.
- Mesurer et rapporter temps avant disponibilité, octets téléchargés, latence des
  premières lectures et débit, sur le même scénario local et distant. Aucun
  engagement chiffré de démarrage n'est pris avant ces mesures.
- Injecter panne S3, corruption, manque d'espace et révocation du propriétaire ;
  vérifier pauses, annulation, absence de perte locale et reprise autorisée.
- Reprendre le dernier point publié sur une autre node avec cache vide et vérifier
  l'environnement complet. Les écritures ultérieures non sauvegardées restent
  soumises au compromis déjà accepté dans l'ADR-0004.
- Vérifier activation locale/distante, migration interrompue, reprise concurrente,
  archivage manuel et restauration d'une archive ancienne à travers les parcours
  existants. Les nodes non activées conservent leur fonctionnement actuel.

L'essai réel FUSE/Firecracker est une condition de poursuite de cet adapter.
Un échec sur la durabilité, le contrôle des E/S bloquées ou les performances devra
être expliqué et conduire à réexaminer l'adapter avant activation, sans déclarer
la fonctionnalité validée sur la seule base de tests simulés.

## Sources

- [ADR-0004 : disque local et sauvegarde asynchrone](adr/0004-local-disk-asynchronous-incremental-backup.md).
- [ADR-0005 : suivi des écritures invité](adr/0005-guest-dm-era-write-tracking.md).
- [Spécification des nodes](DISTRIBUTED-NODES-SPEC.md).
- [Recherche sur le suivi des écritures](INCREMENTAL-VM-BACKUP-WRITE-TRACKING.md).
