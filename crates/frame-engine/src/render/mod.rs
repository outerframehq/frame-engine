use crate::world::World;

// Grid Size for render

const GRID_WIDTH: usize = 20;
const GRID_HIGHT: usize = 10;

pub fn debug_print(world: &World) {
    let mut grid = [['.'; GRID_WIDTH]; GRID_HIGHT];

    for slot in world.positions.iter() {
        if let Some(position) = slot {
            let col = position.x as i32;
            let row = position.y as i32;

            if col >= 0 && col < GRID_WIDTH as i32 && row >= 0 && row < GRID_HIGHT as i32 {
                grid[row as usize][col as usize] = '#';
            }
        }
    }
    for row in grid.iter() {
        let line: String = row.iter().collect();
        println!("{}", line);
    }
}
