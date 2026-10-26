//! The schema.org types a business, organisation or person describes itself with, and their
//! parents (spec 3.5 §7.8 "Sources" `Presence`, S2): a listing typed `Bakery` describes a
//! `FoodEstablishment`, a `LocalBusiness` and an `Organization`.

/// `(type, parent)` for every subject type below the roots `Organization`, `Person` and
/// `Place`, after schema.org 29.
const PARENTS: &[(&str, &str)] = &[
    // Organization.
    ("Airline", "Organization"),
    ("Consortium", "Organization"),
    ("Corporation", "Organization"),
    ("EducationalOrganization", "Organization"),
    ("FundingScheme", "Organization"),
    ("GovernmentOrganization", "Organization"),
    ("LibrarySystem", "Organization"),
    ("LocalBusiness", "Organization"),
    ("MedicalOrganization", "Organization"),
    ("NGO", "Organization"),
    ("NewsMediaOrganization", "Organization"),
    ("OnlineBusiness", "Organization"),
    ("PerformingGroup", "Organization"),
    ("PoliticalParty", "Organization"),
    ("Project", "Organization"),
    ("ResearchOrganization", "Organization"),
    ("SearchRescueOrganization", "Organization"),
    ("SportsOrganization", "Organization"),
    ("WorkersUnion", "Organization"),
    ("CollegeOrUniversity", "EducationalOrganization"),
    ("ElementarySchool", "EducationalOrganization"),
    ("HighSchool", "EducationalOrganization"),
    ("MiddleSchool", "EducationalOrganization"),
    ("Preschool", "EducationalOrganization"),
    ("School", "EducationalOrganization"),
    ("DiagnosticLab", "MedicalOrganization"),
    ("VeterinaryCare", "MedicalOrganization"),
    ("OnlineStore", "OnlineBusiness"),
    ("DanceGroup", "PerformingGroup"),
    ("MusicGroup", "PerformingGroup"),
    ("TheaterGroup", "PerformingGroup"),
    ("FundingAgency", "Project"),
    ("ResearchProject", "Project"),
    ("SportsTeam", "SportsOrganization"),
    // LocalBusiness.
    ("AnimalShelter", "LocalBusiness"),
    ("ArchiveOrganization", "LocalBusiness"),
    ("AutomotiveBusiness", "LocalBusiness"),
    ("ChildCare", "LocalBusiness"),
    ("Dentist", "LocalBusiness"),
    ("DryCleaningOrLaundry", "LocalBusiness"),
    ("EmergencyService", "LocalBusiness"),
    ("EmploymentAgency", "LocalBusiness"),
    ("EntertainmentBusiness", "LocalBusiness"),
    ("FinancialService", "LocalBusiness"),
    ("FoodEstablishment", "LocalBusiness"),
    ("GovernmentOffice", "LocalBusiness"),
    ("HealthAndBeautyBusiness", "LocalBusiness"),
    ("HomeAndConstructionBusiness", "LocalBusiness"),
    ("InternetCafe", "LocalBusiness"),
    ("LegalService", "LocalBusiness"),
    ("Library", "LocalBusiness"),
    ("LodgingBusiness", "LocalBusiness"),
    ("MedicalBusiness", "LocalBusiness"),
    ("ProfessionalService", "LocalBusiness"),
    ("RadioStation", "LocalBusiness"),
    ("RealEstateAgent", "LocalBusiness"),
    ("RecyclingCenter", "LocalBusiness"),
    ("SelfStorage", "LocalBusiness"),
    ("ShoppingCenter", "LocalBusiness"),
    ("SportsActivityLocation", "LocalBusiness"),
    ("Store", "LocalBusiness"),
    ("TelevisionStation", "LocalBusiness"),
    ("TouristInformationCenter", "LocalBusiness"),
    ("TravelAgency", "LocalBusiness"),
    ("AutoBodyShop", "AutomotiveBusiness"),
    ("AutoDealer", "AutomotiveBusiness"),
    ("AutoPartsStore", "AutomotiveBusiness"),
    ("AutoRental", "AutomotiveBusiness"),
    ("AutoRepair", "AutomotiveBusiness"),
    ("AutoWash", "AutomotiveBusiness"),
    ("GasStation", "AutomotiveBusiness"),
    ("MotorcycleDealer", "AutomotiveBusiness"),
    ("MotorcycleRepair", "AutomotiveBusiness"),
    ("FireStation", "EmergencyService"),
    ("Hospital", "EmergencyService"),
    ("PoliceStation", "EmergencyService"),
    ("AdultEntertainment", "EntertainmentBusiness"),
    ("AmusementPark", "EntertainmentBusiness"),
    ("ArtGallery", "EntertainmentBusiness"),
    ("Casino", "EntertainmentBusiness"),
    ("ComedyClub", "EntertainmentBusiness"),
    ("MovieTheater", "EntertainmentBusiness"),
    ("NightClub", "EntertainmentBusiness"),
    ("AccountingService", "FinancialService"),
    ("AutomatedTeller", "FinancialService"),
    ("BankOrCreditUnion", "FinancialService"),
    ("InsuranceAgency", "FinancialService"),
    ("Bakery", "FoodEstablishment"),
    ("BarOrPub", "FoodEstablishment"),
    ("Brewery", "FoodEstablishment"),
    ("CafeOrCoffeeShop", "FoodEstablishment"),
    ("Distillery", "FoodEstablishment"),
    ("FastFoodRestaurant", "FoodEstablishment"),
    ("IceCreamShop", "FoodEstablishment"),
    ("Restaurant", "FoodEstablishment"),
    ("Winery", "FoodEstablishment"),
    ("PostOffice", "GovernmentOffice"),
    ("BeautySalon", "HealthAndBeautyBusiness"),
    ("DaySpa", "HealthAndBeautyBusiness"),
    ("HairSalon", "HealthAndBeautyBusiness"),
    ("HealthClub", "HealthAndBeautyBusiness"),
    ("NailSalon", "HealthAndBeautyBusiness"),
    ("TattooParlor", "HealthAndBeautyBusiness"),
    ("Electrician", "HomeAndConstructionBusiness"),
    ("GeneralContractor", "HomeAndConstructionBusiness"),
    ("HVACBusiness", "HomeAndConstructionBusiness"),
    ("HousePainter", "HomeAndConstructionBusiness"),
    ("Locksmith", "HomeAndConstructionBusiness"),
    ("MovingCompany", "HomeAndConstructionBusiness"),
    ("Plumber", "HomeAndConstructionBusiness"),
    ("RoofingContractor", "HomeAndConstructionBusiness"),
    ("Attorney", "LegalService"),
    ("Notary", "LegalService"),
    ("BedAndBreakfast", "LodgingBusiness"),
    ("Campground", "LodgingBusiness"),
    ("Hostel", "LodgingBusiness"),
    ("Hotel", "LodgingBusiness"),
    ("Motel", "LodgingBusiness"),
    ("Resort", "LodgingBusiness"),
    ("VacationRental", "LodgingBusiness"),
    ("SkiResort", "Resort"),
    ("CommunityHealth", "MedicalBusiness"),
    ("Dermatology", "MedicalBusiness"),
    ("DietNutrition", "MedicalBusiness"),
    ("Emergency", "MedicalBusiness"),
    ("Geriatric", "MedicalBusiness"),
    ("Gynecologic", "MedicalBusiness"),
    ("MedicalClinic", "MedicalBusiness"),
    ("Midwifery", "MedicalBusiness"),
    ("Nursing", "MedicalBusiness"),
    ("Obstetric", "MedicalBusiness"),
    ("Oncologic", "MedicalBusiness"),
    ("Optician", "MedicalBusiness"),
    ("Optometric", "MedicalBusiness"),
    ("Otolaryngologic", "MedicalBusiness"),
    ("Pediatric", "MedicalBusiness"),
    ("Pharmacy", "MedicalBusiness"),
    ("Physician", "MedicalBusiness"),
    ("Physiotherapy", "MedicalBusiness"),
    ("PlasticSurgery", "MedicalBusiness"),
    ("Podiatric", "MedicalBusiness"),
    ("PrimaryCare", "MedicalBusiness"),
    ("Psychiatric", "MedicalBusiness"),
    ("PublicHealth", "MedicalBusiness"),
    ("CovidTestingFacility", "MedicalClinic"),
    ("IndividualPhysician", "Physician"),
    ("PhysiciansOffice", "Physician"),
    ("BowlingAlley", "SportsActivityLocation"),
    ("ExerciseGym", "SportsActivityLocation"),
    ("GolfCourse", "SportsActivityLocation"),
    ("PublicSwimmingPool", "SportsActivityLocation"),
    ("SportsClub", "SportsActivityLocation"),
    ("StadiumOrArena", "SportsActivityLocation"),
    ("TennisComplex", "SportsActivityLocation"),
    ("BikeStore", "Store"),
    ("BookStore", "Store"),
    ("ClothingStore", "Store"),
    ("ComputerStore", "Store"),
    ("ConvenienceStore", "Store"),
    ("DepartmentStore", "Store"),
    ("ElectronicsStore", "Store"),
    ("Florist", "Store"),
    ("FurnitureStore", "Store"),
    ("GardenStore", "Store"),
    ("GroceryStore", "Store"),
    ("HardwareStore", "Store"),
    ("HobbyShop", "Store"),
    ("HomeGoodsStore", "Store"),
    ("JewelryStore", "Store"),
    ("LiquorStore", "Store"),
    ("MensClothingStore", "Store"),
    ("MobilePhoneStore", "Store"),
    ("MovieRentalStore", "Store"),
    ("MusicStore", "Store"),
    ("OfficeEquipmentStore", "Store"),
    ("OutletStore", "Store"),
    ("PawnShop", "Store"),
    ("PetStore", "Store"),
    ("ShoeStore", "Store"),
    ("SportingGoodsStore", "Store"),
    ("TireShop", "Store"),
    ("ToyStore", "Store"),
    ("WholesaleStore", "Store"),
    // Places a presence describes its subject as.
    ("CivicStructure", "Place"),
    ("PlaceOfWorship", "CivicStructure"),
    ("BuddhistTemple", "PlaceOfWorship"),
    ("Church", "PlaceOfWorship"),
    ("CatholicChurch", "Church"),
    ("HinduTemple", "PlaceOfWorship"),
    ("Mosque", "PlaceOfWorship"),
    ("Synagogue", "PlaceOfWorship"),
];

/// The roots of the subject types.
const ROOTS: &[&str] = &["Organization", "Person", "Place"];

/// The canonical name of a schema.org type written as `Bakery`, `schema:Bakery` or
/// `https://schema.org/Bakery`, compared without case.
fn canonical(name: &str) -> Option<&'static str> {
    let bare = name
        .trim()
        .trim_start_matches("https://schema.org/")
        .trim_start_matches("http://schema.org/")
        .trim_start_matches("schema:");
    PARENTS
        .iter()
        .map(|(child, _)| *child)
        .chain(ROOTS.iter().copied())
        .find(|known| known.eq_ignore_ascii_case(bare))
}

/// A schema.org subject type and its ancestors, nearest first: `Bakery` gives
/// `["Bakery", "FoodEstablishment", "LocalBusiness", "Organization"]`. Empty for a type that
/// does not describe a business, organisation, person or place (spec 3.5 §7.8 `Presence`).
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::sources::schema_org_ancestry;
///
/// assert_eq!(
///     schema_org_ancestry("Bakery"),
///     ["Bakery", "FoodEstablishment", "LocalBusiness", "Organization"]
/// );
/// assert_eq!(schema_org_ancestry("https://schema.org/Dentist")[1], "LocalBusiness");
/// assert!(schema_org_ancestry("Recipe").is_empty());
/// ```
pub fn schema_org_ancestry(name: &str) -> Vec<&'static str> {
    let mut chain = Vec::new();
    let mut current = canonical(name);
    while let Some(name) = current {
        if chain.contains(&name) {
            break;
        }
        chain.push(name);
        current = PARENTS.iter().find(|(child, _)| *child == name).map(|(_, parent)| *parent);
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_parent_is_known_and_reaches_a_root() {
        for (child, parent) in PARENTS {
            assert!(canonical(parent).is_some(), "{child}'s parent {parent} is listed");
            let chain = schema_org_ancestry(child);
            assert!(ROOTS.contains(chain.last().unwrap()), "{child}: {chain:?}");
        }
        assert_eq!(schema_org_ancestry("person"), ["Person"]);
        assert_eq!(schema_org_ancestry("schema:Mosque")[3], "Place");
    }
}
