---- MODULE GroupSenderKeys ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    Devices,
    InitialMembers,
    InitialAdmins,
    MaxKey,
    MaxMsg,
    MaxClock,
    SyncLocalSiblings,
    AllowUnauthorizedMembershipMutations,
    BugRepairUsesKeyHistory

ASSUME Devices # {}
ASSUME InitialMembers \subseteq Devices
ASSUME InitialAdmins \subseteq InitialMembers
ASSUME InitialAdmins # {}
ASSUME MaxKey \in Nat \ {0}
ASSUME MaxMsg \in Nat \ {0}
ASSUME MaxClock \in Nat \ {0}
ASSUME SyncLocalSiblings \in BOOLEAN
ASSUME AllowUnauthorizedMembershipMutations \in BOOLEAN
ASSUME BugRepairUsesKeyHistory \in BOOLEAN

Keys == 1..MaxKey
MsgNums == 0..(MaxMsg - 1)
DistPair(s, d, k) == <<s, d, k>>
StateItem(s, k, iter) == <<s, k, iter>>
SnapshotItem(s, d, k, iter) == <<s, d, k, iter>>

DistItem(s, d, k, iter) == <<"dist", s, d, k, iter>>
MsgItem(s, d, k, n, sentAt) == <<"msg", s, d, k, n, sentAt>>
RepairItem(s, d, k, n) == <<"repair", s, d, k, n>>

DistItems ==
    {DistItem(s, d, k, n) :
        s \in Devices, d \in Devices, k \in Keys, n \in MsgNums}

MsgItems ==
    {MsgItem(s, d, k, n, t) :
        s \in Devices, d \in Devices, k \in Keys, n \in MsgNums, t \in 1..MaxClock}

RepairItems ==
    {RepairItem(s, d, k, n) :
        s \in Devices, d \in Devices, k \in Keys, n \in MsgNums}

Items == DistItems \cup MsgItems

ItemKind(i) == i[1]
ItemSender(i) == i[2]
ItemTarget(i) == i[3]
ItemKey(i) == i[4]
ItemNumber(i) == i[5]
MsgSentAt(i) == i[6]

RepairSender(r) == r[2]
RepairTarget(r) == r[3]
RepairKey(r) == r[4]
RepairNumber(r) == r[5]

HasRelayFor(d, bag) ==
    \E i \in bag: ItemTarget(i) = d

HasDistributionRelayFor(d, bag) ==
    \E i \in bag:
        /\ ItemTarget(i) = d
        /\ ItemKind(i) = "dist"

HasKnownDistribution(states, s, k, n) ==
    \E iter \in MsgNums:
        /\ StateItem(s, k, iter) \in states
        /\ iter <= n

EligibleRepairItersFor(s, d, k, n, snapshots) ==
    {iter \in MsgNums:
        /\ SnapshotItem(s, d, k, iter) \in snapshots
        /\ iter <= n}

HistoryRepairItersFor(s, k, n, snapshots) ==
    {iter \in MsgNums:
        /\ \E recipient \in Devices:
            SnapshotItem(s, recipient, k, iter) \in snapshots
        /\ iter <= n}

RepairEligibleIters(r, snapshots) ==
    EligibleRepairItersFor(
        RepairSender(r),
        RepairTarget(r),
        RepairKey(r),
        RepairNumber(r),
        snapshots)

RepairHistoryIters(r, snapshots) ==
    HistoryRepairItersFor(
        RepairSender(r),
        RepairKey(r),
        RepairNumber(r),
        snapshots)

VARIABLES
    clock,
    members,
    admins,
    joinedAt,
    revoked,
    revokedFrom,
    cleanupDone,
    currentKey,
    nextMsg,
    distributedTo,
    repairSnapshots,
    distKnown,
    needDist,
    needMsg,
    relayBag,
    blocked,
    repairPending,
    delivered,
    relayUp,
    badAuthMutation,
    badDecryptWithoutDistribution,
    badUnauthorizedRepair

vars ==
    <<clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
      currentKey, nextMsg, distributedTo, repairSnapshots, distKnown,
      needDist, needMsg, relayBag, blocked, repairPending, delivered,
      relayUp, badAuthMutation, badDecryptWithoutDistribution,
      badUnauthorizedRepair>>

Init ==
    /\ clock = 0
    /\ members = InitialMembers
    /\ admins = InitialAdmins
    /\ joinedAt = [d \in Devices |-> IF d \in InitialMembers THEN 0 ELSE 0]
    /\ revoked = {}
    /\ revokedFrom = [d \in Devices |-> 0]
    /\ cleanupDone = {}
    /\ currentKey = [s \in Devices |-> 1]
    /\ nextMsg = [s \in Devices |-> 0]
    /\ distributedTo = {}
    /\ repairSnapshots = {}
    /\ distKnown = [d \in Devices |-> {}]
    /\ needDist = {}
    /\ needMsg = {}
    /\ relayBag = {}
    /\ blocked = {}
    /\ repairPending = {}
    /\ delivered = {}
    /\ relayUp = TRUE
    /\ badAuthMutation = FALSE
    /\ badDecryptWithoutDistribution = FALSE
    /\ badUnauthorizedRepair = FALSE

NeedsRotation(sender) ==
    \E d \in Devices:
        /\ DistPair(sender, d, currentKey[sender]) \in distributedTo
        /\ d \notin members

AddMember(actor, d) ==
    /\ actor \in Devices
    /\ d \in Devices
    /\ d \notin members
    /\ d \notin revoked
    /\ clock < MaxClock
    /\ (actor \in admins \/ AllowUnauthorizedMembershipMutations)
    /\ clock' = clock + 1
    /\ members' = members \cup {d}
    /\ joinedAt' = [joinedAt EXCEPT ![d] = clock + 1]
    /\ badAuthMutation' = (badAuthMutation \/ ~(actor \in admins))
    /\ UNCHANGED <<
        admins, revoked, revokedFrom, cleanupDone, currentKey, nextMsg,
        distributedTo, repairSnapshots, distKnown, needDist, needMsg,
        relayBag, blocked, repairPending, delivered, relayUp,
        badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

RemoveMember(actor, d) ==
    /\ actor \in Devices
    /\ d \in members
    /\ d # actor
    /\ admins \ {d} # {}
    /\ clock < MaxClock
    /\ (actor \in admins \/ AllowUnauthorizedMembershipMutations)
    /\ clock' = clock + 1
    /\ members' = members \ {d}
    /\ admins' = admins \ {d}
    /\ revoked' = revoked \cup {d}
    /\ revokedFrom' =
        [revokedFrom EXCEPT ![d] = IF @ = 0 THEN clock + 1 ELSE @]
    /\ cleanupDone' = cleanupDone \ {d}
    /\ badAuthMutation' = (badAuthMutation \/ ~(actor \in admins))
    /\ UNCHANGED <<
        joinedAt, currentKey, nextMsg, distributedTo, repairSnapshots,
        distKnown, needDist, needMsg, relayBag, blocked, repairPending,
        delivered, relayUp, badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

AddAdmin(actor, d) ==
    /\ actor \in Devices
    /\ d \in members
    /\ d \notin admins
    /\ clock < MaxClock
    /\ (actor \in admins \/ AllowUnauthorizedMembershipMutations)
    /\ clock' = clock + 1
    /\ admins' = admins \cup {d}
    /\ badAuthMutation' = (badAuthMutation \/ ~(actor \in admins))
    /\ UNCHANGED <<
        members, joinedAt, revoked, revokedFrom, cleanupDone, currentKey,
        nextMsg, distributedTo, repairSnapshots, distKnown, needDist,
        needMsg, relayBag, blocked, repairPending, delivered, relayUp,
        badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

RemoveAdmin(actor, d) ==
    /\ actor \in Devices
    /\ d \in admins
    /\ Cardinality(admins) > 1
    /\ clock < MaxClock
    /\ (actor \in admins \/ AllowUnauthorizedMembershipMutations)
    /\ clock' = clock + 1
    /\ admins' = admins \ {d}
    /\ badAuthMutation' = (badAuthMutation \/ ~(actor \in admins))
    /\ UNCHANGED <<
        members, joinedAt, revoked, revokedFrom, cleanupDone, currentKey,
        nextMsg, distributedTo, repairSnapshots, distKnown, needDist,
        needMsg, relayBag, blocked, repairPending, delivered, relayUp,
        badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

SendMessage(sender) ==
    /\ sender \in members
    /\ members \ {sender} # {}
    /\ clock < MaxClock
    /\ LET rotate == NeedsRotation(sender)
           key == IF rotate THEN currentKey[sender] + 1 ELSE currentKey[sender]
           number == IF rotate THEN 0 ELSE nextMsg[sender]
           remoteRecipients == members \ {sender}
           siblingRecipients == IF SyncLocalSiblings THEN {sender} ELSE {}
           distributionRecipients == remoteRecipients \cup siblingRecipients
           possibleReaders == (Devices \ {sender}) \cup siblingRecipients
           missing == {d \in distributionRecipients: DistPair(sender, d, key) \notin distributedTo}
           newDists == {DistItem(sender, d, key, number) : d \in missing}
           newMsgRelays == {MsgItem(sender, d, key, number, clock + 1) : d \in possibleReaders}
           newMsgNeeds == {MsgItem(sender, d, key, number, clock + 1) : d \in distributionRecipients}
       IN
       /\ (~rotate \/ currentKey[sender] < MaxKey)
       /\ number < MaxMsg
       /\ clock' = clock + 1
       /\ currentKey' = [currentKey EXCEPT ![sender] = key]
       /\ nextMsg' = [nextMsg EXCEPT ![sender] = number + 1]
       /\ distributedTo' =
            distributedTo \cup {DistPair(sender, d, key) : d \in missing}
       /\ repairSnapshots' =
            repairSnapshots \cup {SnapshotItem(sender, d, key, number) : d \in missing}
       /\ needDist' = needDist \cup newDists
       /\ needMsg' = needMsg \cup newMsgNeeds
       /\ relayBag' = relayBag \cup newDists \cup newMsgRelays
    /\ UNCHANGED <<
        members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, blocked, repairPending, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

RetryDist(i) ==
    /\ i \in needDist
    /\ i \notin relayBag
    /\ relayBag' = relayBag \cup {i}
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        currentKey, nextMsg, distributedTo, repairSnapshots, distKnown,
        needDist, needMsg, blocked, repairPending, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

RetryMsg(i) ==
    /\ i \in needMsg
    /\ i \notin relayBag
    /\ relayBag' = relayBag \cup {i}
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        currentKey, nextMsg, distributedTo, repairSnapshots, distKnown,
        needDist, needMsg, blocked, repairPending, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

RespondRepair(r) ==
    /\ r \in repairPending
    /\ RepairTarget(r) \in members
    /\ LET eligible == RepairEligibleIters(r, repairSnapshots)
           history == RepairHistoryIters(r, repairSnapshots)
           responseIters ==
               IF eligible # {}
                   THEN eligible
                   ELSE IF BugRepairUsesKeyHistory THEN history ELSE {}
       IN
       /\ responseIters # {}
       /\ \E iter \in responseIters:
            /\ relayBag' =
                relayBag \cup {
                    DistItem(RepairSender(r), RepairTarget(r), RepairKey(r), iter)
                }
            /\ badUnauthorizedRepair' =
                (badUnauthorizedRepair
                    \/ SnapshotItem(
                            RepairSender(r),
                            RepairTarget(r),
                            RepairKey(r),
                            iter) \notin repairSnapshots)
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        currentKey, nextMsg, distributedTo, repairSnapshots, distKnown,
        needDist, needMsg, blocked, repairPending, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution
      >>

CleanupRemoved(d) ==
    /\ d \in revoked
    /\ d \notin cleanupDone
    /\ cleanupDone' = cleanupDone \cup {d}
    /\ distKnown' = [distKnown EXCEPT ![d] = {}]
    /\ needDist' = {i \in needDist : ItemTarget(i) # d}
    /\ needMsg' = {i \in needMsg : ItemTarget(i) # d}
    /\ blocked' = {i \in blocked : ItemTarget(i) # d}
    /\ repairPending' = {r \in repairPending : RepairTarget(r) # d}
    /\ relayBag' = {i \in relayBag : ItemTarget(i) # d}
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom,
        currentKey, nextMsg, distributedTo, repairSnapshots, delivered,
        relayUp, badAuthMutation, badDecryptWithoutDistribution,
        badUnauthorizedRepair
      >>

RelayDeliverDistToMember(i) ==
    /\ i \in relayBag
    /\ ItemKind(i) = "dist"
    /\ relayUp
    /\ ItemTarget(i) \in members
    /\ LET sender == ItemSender(i)
           target == ItemTarget(i)
           key == ItemKey(i)
           iter == ItemNumber(i)
           newKnown == distKnown[target] \cup {StateItem(sender, key, iter)}
           newlyUnblocked ==
               {m \in blocked:
                    /\ ItemTarget(m) = target
                    /\ ItemSender(m) = sender
                    /\ ItemKey(m) = key
                    /\ HasKnownDistribution(newKnown, sender, key, ItemNumber(m))}
       IN
       /\ relayBag' = relayBag \ {i}
       /\ distKnown' = [distKnown EXCEPT ![target] = newKnown]
       /\ needDist' = needDist \ {i}
       /\ blocked' = blocked \ newlyUnblocked
       /\ delivered' = delivered \cup newlyUnblocked
       /\ needMsg' = needMsg \ newlyUnblocked
       /\ repairPending' =
            repairPending
                \ {RepairItem(ItemSender(m), target, ItemKey(m), ItemNumber(m)) :
                    m \in newlyUnblocked}
       /\ badDecryptWithoutDistribution' =
            (badDecryptWithoutDistribution
                \/ \E m \in newlyUnblocked:
                    ~HasKnownDistribution(newKnown, ItemSender(m), ItemKey(m), ItemNumber(m)))
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        currentKey, nextMsg, distributedTo, repairSnapshots, relayUp,
        badAuthMutation, badUnauthorizedRepair
      >>

RelayDeliverMsgToMember(i) ==
    /\ i \in relayBag
    /\ ItemKind(i) = "msg"
    /\ relayUp
    /\ ItemTarget(i) \in members
    /\ LET sender == ItemSender(i)
           target == ItemTarget(i)
           key == ItemKey(i)
           number == ItemNumber(i)
       IN
       /\ relayBag' = relayBag \ {i}
       /\ IF HasKnownDistribution(distKnown[target], sender, key, number)
             THEN /\ delivered' = delivered \cup {i}
                  /\ needMsg' = needMsg \ {i}
                  /\ blocked' = blocked
                  /\ repairPending' = repairPending \ {RepairItem(sender, target, key, number)}
                  /\ badDecryptWithoutDistribution' = badDecryptWithoutDistribution
             ELSE /\ delivered' = delivered
                  /\ needMsg' = needMsg
                  /\ blocked' = blocked \cup {i}
                  /\ repairPending' = repairPending \cup {RepairItem(sender, target, key, number)}
                  /\ badDecryptWithoutDistribution' = badDecryptWithoutDistribution
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        currentKey, nextMsg, distributedTo, repairSnapshots, distKnown,
        needDist, relayUp, badAuthMutation, badUnauthorizedRepair
      >>

RelayDeliverDropped(i) ==
    /\ i \in relayBag
    /\ relayUp
    /\ ItemTarget(i) \notin members
    /\ relayBag' = relayBag \ {i}
    /\ IF ItemKind(i) = "dist"
          THEN /\ needDist' = needDist \ {i}
               /\ needMsg' = needMsg
          ELSE /\ needMsg' = needMsg \ {i}
               /\ needDist' = needDist
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        currentKey, nextMsg, distributedTo, repairSnapshots, distKnown,
        blocked, repairPending, delivered, relayUp, badAuthMutation,
        badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

RelayDrop(i) ==
    /\ i \in relayBag
    /\ relayBag' = relayBag \ {i}
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        currentKey, nextMsg, distributedTo, repairSnapshots, distKnown,
        needDist, needMsg, blocked, repairPending, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

RelayDuplicate(i) ==
    /\ i \in relayBag
    /\ UNCHANGED vars

RelayPartition ==
    /\ relayUp
    /\ relayUp' = FALSE
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        currentKey, nextMsg, distributedTo, repairSnapshots, distKnown,
        needDist, needMsg, relayBag, blocked, repairPending, delivered,
        badAuthMutation, badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

RelayRecover ==
    /\ ~relayUp
    /\ relayUp' = TRUE
    /\ UNCHANGED <<
        clock, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        currentKey, nextMsg, distributedTo, repairSnapshots, distKnown,
        needDist, needMsg, relayBag, blocked, repairPending, delivered,
        badAuthMutation, badDecryptWithoutDistribution, badUnauthorizedRepair
      >>

RelayDelay ==
    /\ relayBag # {}
    /\ UNCHANGED vars

Stutter == UNCHANGED vars

Next ==
    \/ \E actor \in Devices, d \in Devices: AddMember(actor, d)
    \/ \E actor \in Devices, d \in Devices: RemoveMember(actor, d)
    \/ \E actor \in Devices, d \in Devices: AddAdmin(actor, d)
    \/ \E actor \in Devices, d \in Devices: RemoveAdmin(actor, d)
    \/ \E sender \in Devices: SendMessage(sender)
    \/ \E i \in DistItems: RetryDist(i)
    \/ \E i \in MsgItems: RetryMsg(i)
    \/ \E r \in RepairItems: RespondRepair(r)
    \/ \E d \in Devices: CleanupRemoved(d)
    \/ \E i \in DistItems: RelayDeliverDistToMember(i)
    \/ \E i \in MsgItems: RelayDeliverMsgToMember(i)
    \/ \E i \in Items: RelayDeliverDropped(i)
    \/ \E i \in Items: RelayDrop(i)
    \/ \E i \in Items: RelayDuplicate(i)
    \/ RelayPartition
    \/ RelayRecover
    \/ RelayDelay
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ \A s \in Devices: WF_vars(SendMessage(s))
    /\ \A i \in DistItems: WF_vars(RetryDist(i))
    /\ \A i \in MsgItems: WF_vars(RetryMsg(i))
    /\ \A r \in RepairItems: WF_vars(RespondRepair(r))
    /\ \A d \in Devices: WF_vars(CleanupRemoved(d))
    /\ \A i \in DistItems: SF_vars(RelayDeliverDistToMember(i))
    /\ \A i \in MsgItems: SF_vars(RelayDeliverMsgToMember(i))

SpecUnderRecovery ==
    /\ Spec
    /\ <>[]relayUp

GroupStateValid ==
    /\ admins \subseteq members
    /\ admins # {}

NoUnauthorizedMembershipMutation ==
    ~badAuthMutation

NoDecryptWithoutDistribution ==
    ~badDecryptWithoutDistribution

NoUnauthorizedRepairDistribution ==
    ~badUnauthorizedRepair

NoPreJoinDecrypt ==
    \A m \in delivered:
        /\ joinedAt[ItemTarget(m)] # 0 \/ ItemTarget(m) \in InitialMembers
        /\ MsgSentAt(m) >= joinedAt[ItemTarget(m)]

NoPostRevocationDecrypt ==
    \A m \in delivered:
        revokedFrom[ItemTarget(m)] = 0 \/ MsgSentAt(m) < revokedFrom[ItemTarget(m)]

BlockedPendingDistribution ==
    \A m \in blocked:
        ~HasKnownDistribution(
            distKnown[ItemTarget(m)],
            ItemSender(m),
            ItemKey(m),
            ItemNumber(m))

RepairResponsesUseRecipientSnapshots ==
    \A d \in Devices:
        \A s \in Devices:
            \A k \in Keys:
                \A n \in MsgNums:
                    DistItem(s, d, k, n) \in relayBag
                        => SnapshotItem(s, d, k, n) \in repairSnapshots

NoTransportForRemovedAfterCleanup ==
    \A d \in cleanupDone:
        /\ d \notin members
        /\ d \notin admins
        /\ distKnown[d] = {}
        /\ ~HasDistributionRelayFor(d, relayBag)
        /\ ~HasRelayFor(d, needDist)
        /\ ~HasRelayFor(d, needMsg)
        /\ ~HasRelayFor(d, blocked)
        /\ \A r \in repairPending: RepairTarget(r) # d

NeededDistEventuallyKnownUnderRecovery ==
    \A i \in DistItems:
        [](
            (i \in needDist
              /\ ItemTarget(i) \in members
              /\ revokedFrom[ItemTarget(i)] = 0
              /\ <>[](i \in needDist
                    /\ ItemTarget(i) \in members
                    /\ revokedFrom[ItemTarget(i)] = 0))
                => <>(StateItem(ItemSender(i), ItemKey(i), ItemNumber(i))
                        \in distKnown[ItemTarget(i)])
          )

NeededMsgEventuallyDeliveredUnderRecovery ==
    \A i \in MsgItems:
        [](
            (i \in needMsg
              /\ ItemTarget(i) \in members
              /\ revokedFrom[ItemTarget(i)] = 0
              /\ HasKnownDistribution(
                    distKnown[ItemTarget(i)],
                    ItemSender(i),
                    ItemKey(i),
                    ItemNumber(i))
              /\ <>[](i \in needMsg
                    /\ ItemTarget(i) \in members
                    /\ revokedFrom[ItemTarget(i)] = 0
                    /\ HasKnownDistribution(
                        distKnown[ItemTarget(i)],
                        ItemSender(i),
                        ItemKey(i),
                        ItemNumber(i))))
                => <>(i \in delivered)
          )

RepairableBlockedEventuallyDeliveredUnderRecovery ==
    \A i \in MsgItems:
        [](
            (i \in blocked
              /\ ItemTarget(i) \in members
              /\ revokedFrom[ItemTarget(i)] = 0
              /\ EligibleRepairItersFor(
                    ItemSender(i),
                    ItemTarget(i),
                    ItemKey(i),
                    ItemNumber(i),
                    repairSnapshots) # {}
              /\ <>[](i \in blocked
                    /\ ItemTarget(i) \in members
                    /\ revokedFrom[ItemTarget(i)] = 0
                    /\ EligibleRepairItersFor(
                        ItemSender(i),
                        ItemTarget(i),
                        ItemKey(i),
                        ItemNumber(i),
                        repairSnapshots) # {}))
                => <>(i \in delivered)
          )

RevokedEventuallyPurged ==
    \A d \in Devices:
        [](
            (d \in revoked)
                => <>(d \in cleanupDone
                      /\ d \notin members
                      /\ d \notin admins
                      /\ distKnown[d] = {}
                      /\ ~HasDistributionRelayFor(d, relayBag)
                      /\ ~HasRelayFor(d, needDist)
                      /\ ~HasRelayFor(d, needMsg)
                      /\ ~HasRelayFor(d, blocked)
                      /\ \A r \in repairPending: RepairTarget(r) # d)
          )

====
